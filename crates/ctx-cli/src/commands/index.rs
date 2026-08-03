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
use crate::config;
use crate::output::{compact_json, print_json, JsonOutputFormat};
use crate::semantic::source_epoch_status_report;
use crate::ui::{Document, RenderContext, Ui};

use super::index_dashboard::{render_semantic_disabled_wait, IndexDashboard};

#[cfg(any(test, ctx_pro_test_helper))]
pub(crate) mod dashboard_fixture;

#[derive(Debug, Args)]
pub(crate) struct IndexArgs {
    #[command(subcommand)]
    command: IndexCommand,
}

impl IndexArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            IndexCommand::Watch(args) => args.format == IndexWatchFormat::Jsonl,
            IndexCommand::Wait(args) => args.format.is_json(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    #[command(about = "Watch local indexing progress until ready")]
    Watch(IndexWatchArgs),
    #[command(about = "Wait until local indexing reaches a ready state")]
    Wait(IndexWaitArgs),
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

pub(crate) fn run_index(
    args: IndexArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    match args.command {
        IndexCommand::Watch(args) => {
            telemetry.operation = Some(IndexOperation::Watch);
            run_index_watch(args, &data_root, quiet, telemetry, ui)
        }
        IndexCommand::Wait(args) => {
            telemetry.operation = Some(IndexOperation::Wait);
            run_index_wait(args, &data_root, quiet, telemetry, ui)
        }
    }
}

fn run_index_watch(
    args: IndexWatchArgs,
    data_root: &Path,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let interval = Duration::from_secs(args.interval_seconds);
    let jsonl_output = args.format == IndexWatchFormat::Jsonl;
    let mut output = index_watch_output(ui);
    loop {
        let status = index_readiness_snapshot(data_root)?;
        let selection = IndexSelection::default_for(&status);
        record_index_telemetry(telemetry, &status);
        if jsonl_output {
            output.print_json(&status)?;
        } else if !quiet {
            output.print_human(&status)?;
        }
        if let Some(message) = index_terminal_error(&status, selection) {
            return Err(forward_index_terminal_error(
                message,
                !jsonl_output && !quiet,
            ));
        }
        if index_ready(&status, selection) {
            break;
        }
        thread::sleep(interval);
    }
    Ok(())
}

fn index_watch_output(ui: &mut Ui) -> IndexWatchOutput<&mut (dyn io::Write + Send)> {
    let context = *ui.stdout_context();
    IndexWatchOutput::new(ui.stdout_writer(), context)
}

struct IndexWatchOutput<W> {
    writer: W,
    context: RenderContext,
    interactive: bool,
    rendered_lines: usize,
    dashboard: IndexDashboard,
}

impl<W: io::Write> IndexWatchOutput<W> {
    fn new(writer: W, context: RenderContext) -> Self {
        Self {
            writer,
            context,
            interactive: context.is_terminal(),
            rendered_lines: 0,
            dashboard: IndexDashboard,
        }
    }

    #[cfg(test)]
    fn for_test(writer: W, interactive: bool, terminal_width: usize) -> Self {
        let test_context = if interactive {
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stdout, terminal_width)
        } else {
            crate::ui::TestContext::pipe(crate::ui::StreamKind::Stdout)
        };
        Self::new(writer, RenderContext::for_test(test_context))
    }

    fn print_json(&mut self, status: &Value) -> Result<()> {
        writeln!(self.writer, "{}", serde_json::to_string(status)?)?;
        self.writer.flush()?;
        Ok(())
    }

    fn print_human(&mut self, status: &Value) -> io::Result<()> {
        let document = self.dashboard.render(status, &self.context);
        let frame = document.render(&self.context);
        if !self.interactive {
            self.writer.write_all(frame.as_bytes())?;
            writeln!(self.writer)?;
            return self.writer.flush();
        }

        let lines = frame.lines().collect::<Vec<_>>();
        if self.rendered_lines == 0 {
            self.writer.write_all(frame.as_bytes())?;
            self.rendered_lines = lines.len();
            return self.writer.flush();
        }
        write!(self.writer, "\u{1b}[{}A", self.rendered_lines)?;
        let previous_lines = self.rendered_lines;
        let height = self.rendered_lines.max(lines.len());
        for row in 0..height {
            write!(self.writer, "\r\u{1b}[2K")?;
            if let Some(line) = lines.get(row) {
                write!(self.writer, "{line}")?;
            }
            writeln!(self.writer)?;
        }
        if previous_lines > lines.len() {
            write!(
                self.writer,
                "\u{1b}[{}A",
                previous_lines.saturating_sub(lines.len())
            )?;
        }
        self.rendered_lines = lines.len();
        self.writer.flush()
    }
}

#[derive(Default)]
struct IndexWaitHumanOutput {
    dashboard: IndexDashboard,
    last_frame: Option<String>,
}

impl IndexWaitHumanOutput {
    fn print(&mut self, ui: &mut Ui, status: &Value) -> Result<()> {
        let (document, frame) = self.render(ui, status);
        ui.write_stdout(&document)?;
        self.last_frame = Some(frame);
        Ok(())
    }

    fn print_final(&mut self, ui: &mut Ui, status: &Value) -> Result<()> {
        let (document, frame) = self.render(ui, status);
        if self.last_frame.as_ref() != Some(&frame) {
            ui.write_stdout(&document)?;
            self.last_frame = Some(frame);
        }
        Ok(())
    }

    fn render(&mut self, ui: &Ui, status: &Value) -> (Document, String) {
        let context = *ui.stdout_context();
        let document = self.dashboard.render(status, &context);
        let frame = document.render(&context);
        (document, frame)
    }
}

fn run_index_wait(
    args: IndexWaitArgs,
    data_root: &Path,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let explicit_selection = IndexSelection::from_wait_args(&args);
    let interval = Duration::from_secs(args.interval_seconds);
    let started = Instant::now();
    let mut human_output = IndexWaitHumanOutput::default();
    loop {
        let status = index_readiness_snapshot(data_root)?;
        let selection = explicit_selection.unwrap_or_else(|| IndexSelection::default_for(&status));
        telemetry.wait_lexical = Some(selection.lexical);
        telemetry.wait_semantic = Some(selection.semantic);
        record_index_telemetry(telemetry, &status);
        if let Some(message) = index_terminal_error(&status, selection) {
            telemetry.wait_outcome = Some(WaitOutcome::Blocked);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "blocked"))?;
            } else if !quiet {
                if selection.semantic
                    && !bool_at(&status, &["semantic", "enabled"])
                    && lexical_ready(&status)
                {
                    let document = render_semantic_disabled_wait(&status, ui.stdout_context());
                    ui.write_stdout(&document)?;
                } else {
                    human_output.print(ui, &status)?;
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
                print_json(index_wait_json(status, selection, "ready"))?;
            } else if !quiet {
                human_output.print(ui, &status)?;
            }
            return Ok(());
        }
        if args
            .timeout_seconds
            .is_some_and(|timeout| started.elapsed() >= Duration::from_secs(timeout))
        {
            telemetry.wait_outcome = Some(WaitOutcome::Timeout);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "timeout"))?;
            } else if !quiet {
                human_output.print_final(ui, &status)?;
            }
            return Err(anyhow!(
                "ctx index wait timed out before indexing was ready"
            ));
        }
        if !quiet && !args.format.is_json() {
            human_output.print(ui, &status)?;
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

fn index_readiness_snapshot(data_root: &Path) -> Result<Value> {
    let config = config::AppConfig::load(data_root)?;
    let source = source_epoch_status_report(data_root, &config)?;
    let source_lexical = &source.report["lexical"];
    let source_semantic = &source.report["semantic"];
    let semantic_flat = &source_semantic["flat_f32"];
    let source_daemon = &source.report["daemon"];
    Ok(compact_json(json!({
        "schema_version": 1,
        "initialized": source.initialized,
        "lexical": {
            "status": source_lexical.get("status"),
            "reason": source_lexical.get("reason"),
            "generation_id": source_lexical.get("generation_id"),
            "indexed_items": source.indexed_items,
            "indexed_sessions": source.indexed_sessions,
            "indexed_events": source.indexed_events,
            "indexed_sources": source.indexed_sources,
            "certified_source_bytes": source_lexical.get("certified_source_bytes"),
        },
        "refresh": {
            "status": source.report["refresh"].get("status"),
            "reason": source.report["refresh"].get("reason"),
            "request_state": source.report["refresh"].get("request_state"),
            "request_id": source.report["refresh"].get("request_id"),
            "published_generation": source.report["refresh"].get("published_generation"),
            "generation_id": source.report["refresh"].get("generation_id"),
            "generation_matches": source.report["refresh"].get("generation_matches"),
            "certified_source_count": source.report["refresh"].get("certified_source_count"),
            "certified_source_bytes": source.report["refresh"].get("certified_source_bytes"),
            "progress": source.report["refresh"].get("progress"),
        },
        "semantic": {
            "status": source_semantic.get("status"),
            "reason": source_semantic.get("reason"),
            "enabled": source_semantic.get("enabled"),
            "coverage": {
                "searchable_items": semantic_flat.get("semantic_documents"),
                "embedded_items": semantic_flat.get("active_events"),
                "embedded_chunks": semantic_flat.get("active_chunks"),
            },
        },
        "daemon": {
            "status": source_daemon.get("status"),
            "running": source_daemon.get("running"),
            "jobs": {
                "semantic_index": source_daemon.get("jobs").and_then(|jobs| jobs.get("semantic_index")),
            },
        },
        "local_only": true,
        "read_only": true,
    })))
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
    matches!(state, "pending" | "queued" | "running")
}

fn forward_index_terminal_error(message: String, human_output_rendered: bool) -> anyhow::Error {
    if human_output_rendered {
        crate::dispatch::rendered_cli_error()
    } else {
        anyhow!(message)
    }
}

fn index_wait_json(status: Value, selection: IndexSelection, wait_status: &str) -> Value {
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
        "read_only": true,
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

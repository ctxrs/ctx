use std::{
    io::{self, IsTerminal as _},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::analytics::{count_bucket, IndexOperation, IndexState, IndexTelemetry, WaitOutcome};
use crate::config::{self, CONFIG_FILE};
use crate::output::{compact_json, print_json, JsonOutputFormat};
use crate::semantic::source_epoch_status_report;

use super::index_dashboard::{color_enabled, terminal_width, IndexDashboard};

#[derive(Debug, Args)]
pub(crate) struct IndexArgs {
    #[command(subcommand)]
    command: IndexCommand,
}

impl IndexArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            IndexCommand::Status(args) => args.format.is_json(),
            IndexCommand::Watch(args) => args.format == IndexWatchFormat::Jsonl,
            IndexCommand::Wait(args) => args.format.is_json(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    #[command(about = "Show local indexing progress once")]
    Status(IndexStatusArgs),
    #[command(about = "Watch local indexing progress until ready")]
    Watch(IndexWatchArgs),
    #[command(about = "Wait until local indexing reaches a ready state")]
    Wait(IndexWaitArgs),
}

#[derive(Debug, Args)]
struct IndexStatusArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
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
) -> Result<()> {
    match args.command {
        IndexCommand::Status(args) => {
            telemetry.operation = Some(IndexOperation::Status);
            run_index_status(args, &data_root, quiet, telemetry)
        }
        IndexCommand::Watch(args) => {
            telemetry.operation = Some(IndexOperation::Watch);
            run_index_watch(args, &data_root, quiet, telemetry)
        }
        IndexCommand::Wait(args) => {
            telemetry.operation = Some(IndexOperation::Wait);
            run_index_wait(args, &data_root, quiet, telemetry)
        }
    }
}

fn run_index_status(
    args: IndexStatusArgs,
    data_root: &Path,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
) -> Result<()> {
    let status = index_status_snapshot(data_root)?;
    record_index_telemetry(telemetry, &status);
    if args.format.is_json() {
        print_json(status)?;
    } else if !quiet {
        print_index_status_human(&status);
    }
    Ok(())
}

fn run_index_watch(
    args: IndexWatchArgs,
    data_root: &Path,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
) -> Result<()> {
    let interval = Duration::from_secs(args.interval_seconds);
    let stdout = io::stdout();
    let jsonl_output = args.format == IndexWatchFormat::Jsonl;
    let interactive = !jsonl_output && stdout.is_terminal();
    let mut output = IndexWatchOutput::new(stdout.lock(), interactive);
    loop {
        let status = index_status_snapshot(data_root)?;
        let selection = IndexSelection::default_for(&status);
        record_index_telemetry(telemetry, &status);
        if jsonl_output {
            output.print_json(&status)?;
        } else if !quiet {
            output.print_human(&status)?;
        }
        if index_ready(&status, selection) {
            break;
        }
        if let Some(message) = index_terminal_error(&status, selection) {
            return Err(anyhow!(message));
        }
        thread::sleep(interval);
    }
    Ok(())
}

struct IndexWatchOutput<W> {
    writer: W,
    interactive: bool,
    styled: bool,
    rendered_lines: usize,
    terminal_width_override: Option<usize>,
    dashboard: IndexDashboard,
}

impl<W: io::Write> IndexWatchOutput<W> {
    fn new(writer: W, interactive: bool) -> Self {
        Self {
            writer,
            interactive,
            styled: interactive && color_enabled(),
            rendered_lines: 0,
            terminal_width_override: None,
            dashboard: IndexDashboard::default(),
        }
    }

    #[cfg(test)]
    fn for_test(writer: W, interactive: bool, terminal_width: usize) -> Self {
        Self {
            writer,
            interactive,
            styled: false,
            rendered_lines: 0,
            terminal_width_override: Some(terminal_width),
            dashboard: IndexDashboard::default(),
        }
    }

    fn print_json(&mut self, status: &Value) -> Result<()> {
        writeln!(self.writer, "{}", serde_json::to_string(status)?)?;
        self.writer.flush()?;
        Ok(())
    }

    fn print_human(&mut self, status: &Value) -> io::Result<()> {
        let width = self.terminal_width_override.unwrap_or_else(terminal_width);
        let frame = self.dashboard.render(status, width, self.styled);
        if !self.interactive {
            writeln!(self.writer, "{frame}")?;
            writeln!(self.writer)?;
            return self.writer.flush();
        }

        let lines = frame.lines().collect::<Vec<_>>();
        if self.rendered_lines == 0 {
            writeln!(self.writer, "{frame}")?;
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

fn run_index_wait(
    args: IndexWaitArgs,
    data_root: &Path,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
) -> Result<()> {
    let explicit_selection = IndexSelection::from_wait_args(&args);
    let interval = Duration::from_secs(args.interval_seconds);
    let started = Instant::now();
    loop {
        let status = index_status_snapshot(data_root)?;
        let selection = explicit_selection.unwrap_or_else(|| IndexSelection::default_for(&status));
        telemetry.wait_lexical = Some(selection.lexical);
        telemetry.wait_semantic = Some(selection.semantic);
        record_index_telemetry(telemetry, &status);
        if index_ready(&status, selection) {
            telemetry.wait_outcome = Some(WaitOutcome::Ready);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "ready"))?;
            } else if !quiet {
                print_index_status_human(&status);
            }
            return Ok(());
        }
        if let Some(message) = index_terminal_error(&status, selection) {
            telemetry.wait_outcome = Some(WaitOutcome::Blocked);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "blocked"))?;
            }
            return Err(anyhow!(message));
        }
        if args
            .timeout_seconds
            .is_some_and(|timeout| started.elapsed() >= Duration::from_secs(timeout))
        {
            telemetry.wait_outcome = Some(WaitOutcome::Timeout);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "timeout"))?;
            }
            return Err(anyhow!(
                "ctx index wait timed out before indexing was ready"
            ));
        }
        if !quiet && !args.format.is_json() {
            print_index_watch_human(&status);
            println!();
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
    telemetry.indexed_items = Some(count_bucket(
        usize_at(status, &["lexical", "indexed_items"]) as u64,
    ));
    telemetry.inventory_units = Some(count_bucket(
        usize_at(status, &["lexical", "inventory_units"]) as u64,
    ));
    telemetry.pending_inventory_units = Some(count_bucket(usize_at(
        status,
        &["lexical", "pending_inventory_units"],
    ) as u64));
    telemetry.failed_inventory_units = Some(count_bucket(usize_at(
        status,
        &["lexical", "failed_inventory_units"],
    ) as u64));
    telemetry.stale_inventory_units = Some(count_bucket(usize_at(
        status,
        &["lexical", "stale_inventory_units"],
    ) as u64));
}

fn index_status_snapshot(data_root: &Path) -> Result<Value> {
    let config_path = data_root.join(CONFIG_FILE);
    let config = config::AppConfig::load(data_root)?;
    let source = source_epoch_status_report(data_root, &config)?;
    let initialized = source.initialized;
    let indexed_items = source.indexed_items.unwrap_or(0) as usize;
    let indexed_sessions = source.indexed_sessions.unwrap_or(0) as usize;
    let indexed_events = source.indexed_events.unwrap_or(0) as usize;
    let inventory_units = source.indexed_sources.unwrap_or(0) as usize;
    let source_lexical = &source.report["lexical"];
    let source_lexical_status = source_lexical
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let total_source_bytes = source_lexical
        .get("certified_source_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completed_source_bytes = if source_lexical_status == "ready" {
        total_source_bytes
    } else {
        0
    };
    let pending_inventory_units = if source_lexical_status == "pending" {
        inventory_units.max(1)
    } else {
        0
    };
    let failed_inventory_units = usize::from(initialized && source_lexical_status == "unavailable");
    let stale_inventory_units = 0;
    let lexical_status = lexical_index_status(
        initialized,
        indexed_items,
        inventory_units,
        pending_inventory_units,
        failed_inventory_units,
    );
    let mut semantic = source.report["semantic"].clone();
    if let Some(object) = semantic.as_object_mut() {
        let flat = object.get("flat_f32");
        let embedded_items = flat
            .and_then(|value| value.get("active_events"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let embedded_chunks = flat
            .and_then(|value| value.get("active_chunks"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        object.insert(
            "coverage".to_owned(),
            json!({
                "embedded_items": embedded_items,
                "searchable_items": indexed_events,
                "embedded_chunks": embedded_chunks,
                "queued_items_estimate": (indexed_events as u64).saturating_sub(embedded_items),
            }),
        );
    }
    let daemon = source.report["daemon"].clone();
    Ok(compact_json(json!({
        "schema_version": 2,
        "initialized": initialized,
        "data_root": data_root,
        "index_path": source_lexical.get("path"),
        "config_path": config_path,
        "lexical": {
            "status": lexical_status,
            "source_status": source_lexical_status,
            "generation_id": source_lexical.get("generation_id"),
            "indexed_items": indexed_items,
            "indexed_sessions": indexed_sessions,
            "indexed_events": indexed_events,
            "completed_source_bytes": completed_source_bytes,
            "total_source_bytes": total_source_bytes,
            "inventory_units": inventory_units,
            "pending_inventory_units": pending_inventory_units,
            "failed_inventory_units": failed_inventory_units,
            "stale_inventory_units": stale_inventory_units,
        },
        "history_epoch": source.report["history_epoch"].clone(),
        "refresh": source.report["refresh"].clone(),
        "semantic": semantic,
        "daemon": daemon,
        "local_only": true,
        "read_only": true,
    })))
}

fn lexical_index_status(
    initialized: bool,
    indexed_items: usize,
    inventory_units: usize,
    pending_inventory_units: usize,
    failed_inventory_units: usize,
) -> &'static str {
    if !initialized {
        "missing"
    } else if failed_inventory_units > 0 {
        "failed"
    } else if pending_inventory_units > 0 && indexed_items > 0 {
        "partial"
    } else if pending_inventory_units > 0 {
        "pending"
    } else if indexed_items > 0 {
        "ready"
    } else if inventory_units == 0 {
        "empty"
    } else {
        "ready"
    }
}

fn print_index_status_human(status: &Value) {
    println!("data_root: {}", string_at(status, &["data_root"], ""));
    println!("initialized: {}", bool_at(status, &["initialized"]));
    println!(
        "lexical_status: {}",
        string_at(status, &["lexical", "status"], "unknown")
    );
    println!(
        "lexical_indexed_items: {}",
        usize_at(status, &["lexical", "indexed_items"])
    );
    println!(
        "lexical_pending_units: {}",
        usize_at(status, &["lexical", "pending_inventory_units"])
    );
    println!("semantic_status: {}", semantic_job_status(status));
    println!(
        "semantic_embedded_items: {}",
        usize_at(status, &["semantic", "coverage", "embedded_items"])
    );
    println!(
        "semantic_searchable_items: {}",
        usize_at(status, &["semantic", "coverage", "searchable_items"])
    );
    println!(
        "semantic_queued_items_estimate: {}",
        usize_at(status, &["semantic", "coverage", "queued_items_estimate"])
    );
    println!(
        "daemon_status: {}",
        string_at(status, &["daemon", "status"], "unknown")
    );
    let daemon_reason = string_at(status, &["daemon", "reason"], "");
    if !daemon_reason.is_empty() {
        println!("daemon_reason: {daemon_reason}");
    }
    if bool_at(status, &["daemon", "recoverable"]) {
        println!("daemon_recoverable: true");
    }
    println!(
        "daemon_running: {}",
        bool_at(status, &["daemon", "running"])
    );
    println!("read_only: true");
}

fn print_index_watch_human(status: &Value) {
    let mut dashboard = IndexDashboard::default();
    println!("{}", dashboard.render(status, 80, false));
}

#[derive(Debug, Clone, Copy)]
struct IndexSelection {
    lexical: bool,
    semantic: bool,
}

impl IndexSelection {
    fn all() -> Self {
        Self {
            lexical: true,
            semantic: true,
        }
    }

    fn from_wait_args(args: &IndexWaitArgs) -> Option<Self> {
        if args.all {
            Some(Self::all())
        } else if args.lexical || args.semantic {
            Some(Self {
                lexical: args.lexical,
                semantic: args.semantic,
            })
        } else {
            None
        }
    }

    fn default_for(status: &Value) -> Self {
        Self {
            lexical: true,
            semantic: bool_at(status, &["semantic", "enabled"]),
        }
    }
}

fn index_ready(status: &Value, selection: IndexSelection) -> bool {
    (!selection.lexical || lexical_ready(status)) && (!selection.semantic || semantic_ready(status))
}

fn lexical_ready(status: &Value) -> bool {
    matches!(
        string_at(status, &["lexical", "status"], "unknown").as_str(),
        "ready" | "empty"
    )
}

fn semantic_ready(status: &Value) -> bool {
    matches!(semantic_job_status(status).as_str(), "ready" | "empty")
}

fn index_terminal_error(status: &Value, selection: IndexSelection) -> Option<String> {
    if selection.lexical && string_at(status, &["lexical", "status"], "unknown") == "missing" {
        return Some("ctx index does not exist yet; run `ctx setup` first".to_owned());
    }
    if selection.lexical
        && string_at(status, &["lexical", "status"], "unknown") == "failed"
        && !bool_at(status, &["daemon", "running"])
    {
        return Some(
            "one or more history files could not be indexed; run `ctx doctor` for details"
                .to_owned(),
        );
    }
    if selection.semantic {
        let semantic_status = semantic_job_status(status);
        let reason = string_at(status, &["daemon", "jobs", "semantic_index", "reason"], "");
        if semantic_status == "skipped" && reason == "model_cache_missing" {
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
    if !index_ready(status, selection)
        && string_at(status, &["daemon", "status"], "unknown") == "failed"
        && !bool_at(status, &["daemon", "running"])
    {
        return Some(
            "background indexing stopped before the index was ready; run `ctx doctor` for details"
                .to_owned(),
        );
    }
    None
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
        "index": status,
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

fn usize_at(value: &Value, path: &[&str]) -> usize {
    value_at(value, path)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
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

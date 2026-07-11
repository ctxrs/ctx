use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use ctx_history_core::database_path;

use crate::config::{self, CONFIG_FILE};
use crate::output::{compact_json, print_json};
use crate::progress::format_count;
use crate::semantic::{
    daemon_report, semantic_worker_report_cached, semantic_worker_report_configured_json,
};
use crate::store_util::open_existing_store_snapshot_read_only;

#[derive(Debug, Args)]
pub(crate) struct IndexArgs {
    #[command(subcommand)]
    command: IndexCommand,
}

impl IndexArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            IndexCommand::Status(args) => args.json,
            IndexCommand::Watch(args) => args.json,
            IndexCommand::Wait(args) => args.json,
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
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct IndexWatchArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 2, value_parser = parse_positive_seconds)]
    interval_seconds: u64,
}

#[derive(Debug, Args)]
struct IndexWaitArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, help = "Wait for lexical SQLite indexing")]
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

pub(crate) fn run_index(args: IndexArgs, data_root: PathBuf, quiet: bool) -> Result<()> {
    match args.command {
        IndexCommand::Status(args) => run_index_status(args, &data_root, quiet),
        IndexCommand::Watch(args) => run_index_watch(args, &data_root, quiet),
        IndexCommand::Wait(args) => run_index_wait(args, &data_root, quiet),
    }
}

fn run_index_status(args: IndexStatusArgs, data_root: &Path, quiet: bool) -> Result<()> {
    let status = index_status_snapshot(data_root)?;
    if args.json {
        print_json(status)?;
    } else if !quiet {
        print_index_status_human(&status);
    }
    Ok(())
}

fn run_index_watch(args: IndexWatchArgs, data_root: &Path, quiet: bool) -> Result<()> {
    let interval = Duration::from_secs(args.interval_seconds);
    loop {
        let status = index_status_snapshot(data_root)?;
        if args.json {
            println!("{}", serde_json::to_string(&status)?);
        } else if !quiet {
            print_index_watch_human(&status);
            println!();
        }
        if index_ready(&status, IndexSelection::all()) {
            break;
        }
        if let Some(message) = index_terminal_error(&status, IndexSelection::all()) {
            return Err(anyhow!(message));
        }
        thread::sleep(interval);
    }
    Ok(())
}

fn run_index_wait(args: IndexWaitArgs, data_root: &Path, quiet: bool) -> Result<()> {
    let explicit_selection = IndexSelection::from_wait_args(&args);
    let interval = Duration::from_secs(args.interval_seconds);
    let started = Instant::now();
    loop {
        let status = index_status_snapshot(data_root)?;
        let selection = explicit_selection.unwrap_or_else(|| IndexSelection::default_for(&status));
        if index_ready(&status, selection) {
            if args.json {
                print_json(index_wait_json(status, selection, "ready"))?;
            } else if !quiet {
                print_index_status_human(&status);
            }
            return Ok(());
        }
        if let Some(message) = index_terminal_error(&status, selection) {
            if args.json {
                print_json(index_wait_json(status, selection, "blocked"))?;
            }
            return Err(anyhow!(message));
        }
        if args
            .timeout_seconds
            .is_some_and(|timeout| started.elapsed() >= Duration::from_secs(timeout))
        {
            if args.json {
                print_json(index_wait_json(status, selection, "timeout"))?;
            }
            return Err(anyhow!(
                "ctx index wait timed out before indexing was ready"
            ));
        }
        if !quiet && !args.json {
            print_index_watch_human(&status);
            println!();
        }
        thread::sleep(interval);
    }
}

fn index_status_snapshot(data_root: &Path) -> Result<Value> {
    let db_path = database_path(data_root.to_path_buf());
    let initialized = db_path.exists();
    let config_path = data_root.join(CONFIG_FILE);
    let config = config::AppConfig::load(data_root)?;
    let (
        indexed_items,
        indexed_sessions,
        indexed_events,
        inventory_units,
        pending_inventory_units,
        failed_inventory_units,
        stale_inventory_units,
        semantic,
        daemon,
    ) = if initialized {
        let store = open_existing_store_snapshot_read_only(&db_path, "ctx index status")?;
        let indexed_counts = store.indexed_history_counts()?;
        let catalog_counts = store.catalog_session_counts()?;
        let source_import_file_counts = store.source_import_file_counts()?;
        let inventory_units = catalog_counts
            .total
            .saturating_add(source_import_file_counts.total);
        let pending_inventory_units = catalog_counts
            .pending
            .saturating_add(source_import_file_counts.pending);
        let failed_inventory_units = catalog_counts
            .failed
            .saturating_add(source_import_file_counts.failed);
        let stale_inventory_units = catalog_counts
            .stale
            .saturating_add(source_import_file_counts.stale);
        let semantic_report = semantic_worker_report_cached(data_root, Some(&store))?;
        let daemon = daemon_report(data_root, &semantic_report);
        (
            indexed_counts.items(),
            indexed_counts.sessions,
            indexed_counts.events,
            inventory_units,
            pending_inventory_units,
            failed_inventory_units,
            stale_inventory_units,
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    } else {
        let semantic_report = semantic_worker_report_cached(data_root, None)?;
        let daemon = daemon_report(data_root, &semantic_report);
        (
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    };
    let lexical_status = lexical_index_status(
        initialized,
        indexed_items,
        inventory_units,
        pending_inventory_units,
    );
    Ok(compact_json(json!({
        "schema_version": 1,
        "initialized": initialized,
        "data_root": data_root,
        "database_path": db_path,
        "config_path": config_path,
        "lexical": {
            "status": lexical_status,
            "indexed_items": indexed_items,
            "indexed_sessions": indexed_sessions,
            "indexed_events": indexed_events,
            "inventory_units": inventory_units,
            "pending_inventory_units": pending_inventory_units,
            "failed_inventory_units": failed_inventory_units,
            "stale_inventory_units": stale_inventory_units,
        },
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
) -> &'static str {
    if !initialized {
        "missing"
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
    let lexical_total = usize_at(status, &["lexical", "inventory_units"]);
    let lexical_pending = usize_at(status, &["lexical", "pending_inventory_units"]);
    let lexical_done = lexical_total.saturating_sub(lexical_pending);
    let semantic_done = usize_at(status, &["semantic", "coverage", "embedded_items"]);
    let semantic_total = usize_at(status, &["semantic", "coverage", "searchable_items"]);
    println!(
        "lexical  [{}] {}/{} units ({})",
        progress_bar(lexical_done, lexical_total),
        format_count(lexical_done),
        format_count(lexical_total),
        string_at(status, &["lexical", "status"], "unknown")
    );
    println!(
        "semantic [{}] {}/{} events, {} chunks ({})",
        progress_bar(semantic_done, semantic_total),
        format_count(semantic_done),
        format_count(semantic_total),
        format_count(usize_at(
            status,
            &["semantic", "coverage", "embedded_chunks"]
        )),
        semantic_job_status(status)
    );
    println!(
        "daemon   {} running={}",
        string_at(status, &["daemon", "status"], "unknown"),
        bool_at(status, &["daemon", "running"])
    );
    let daemon_reason = string_at(status, &["daemon", "reason"], "");
    if !daemon_reason.is_empty() {
        println!("         reason={daemon_reason}");
    }
}

fn progress_bar(done: usize, total: usize) -> String {
    const WIDTH: usize = 20;
    if total == 0 {
        return "-".repeat(WIDTH);
    }
    let filled = done.saturating_mul(WIDTH).saturating_div(total).min(WIDTH);
    format!("{}{}", "#".repeat(filled), "-".repeat(WIDTH - filled))
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
    None
}

fn index_wait_json(status: Value, selection: IndexSelection, wait_status: &str) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": wait_status,
        "selection": {
            "lexical": selection.lexical,
            "semantic": selection.semantic,
        },
        "index": status,
        "local_only": true,
        "read_only": true,
    }))
}

fn semantic_job_status(status: &Value) -> String {
    string_at(
        status,
        &["daemon", "jobs", "semantic_index", "status"],
        "unknown",
    )
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

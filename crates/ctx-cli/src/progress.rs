use std::{
    io::{IsTerminal, Write},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use clap::ValueEnum;
use serde_json::json;

use ctx_history_capture::{
    CodexSessionImportProgress, CodexSessionImportProgressCallback, ProviderImportSummary,
};
use ctx_history_core::CaptureProvider;

use crate::commands::import::{source_error_reason, SourceStats};
use crate::provider_sources::SourceInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProgressArg {
    Auto,
    Plain,
    Json,
    None,
}
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceProgressSnapshot {
    pub(crate) completed_bytes: u64,
    pub(crate) total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressRenderMode {
    None,
    Plain { interactive: bool },
    Json,
}

#[derive(Debug)]
struct ProgressState {
    started: Instant,
    last_emit: Option<Instant>,
    last_line_len: usize,
}

#[derive(Clone)]
pub(crate) struct ProgressReporter {
    mode: ProgressRenderMode,
    operation: &'static str,
    total_bytes: u64,
    state: Arc<Mutex<ProgressState>>,
}

impl ProgressReporter {
    pub(crate) fn new(
        arg: ProgressArg,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        let stderr_is_terminal = std::io::stderr().is_terminal();
        let mode = match arg {
            ProgressArg::None => ProgressRenderMode::None,
            ProgressArg::Json => ProgressRenderMode::Json,
            ProgressArg::Plain => ProgressRenderMode::Plain {
                interactive: stderr_is_terminal,
            },
            ProgressArg::Auto if json_output || !stderr_is_terminal => ProgressRenderMode::None,
            ProgressArg::Auto => ProgressRenderMode::Plain { interactive: true },
        };
        Self {
            mode,
            operation,
            total_bytes,
            state: Arc::new(Mutex::new(ProgressState {
                started: Instant::now(),
                last_emit: None,
                last_line_len: 0,
            })),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.mode != ProgressRenderMode::None
    }

    pub(crate) fn message(&self, phase: &'static str, message: impl Into<String>) {
        if !self.is_enabled() {
            return;
        }
        let message = message.into();
        self.emit(ProgressLine {
            phase,
            message,
            completed_bytes: 0,
            total_bytes: self.total_bytes,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: false,
            force: true,
        });
    }

    pub(crate) fn done(
        &self,
        phase: &'static str,
        message: impl Into<String>,
        completed_bytes: u64,
    ) {
        if !self.is_enabled() {
            return;
        }
        self.emit(ProgressLine {
            phase,
            message: message.into(),
            completed_bytes,
            total_bytes: self.total_bytes.max(completed_bytes),
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: true,
            force: true,
        });
    }

    pub(crate) fn finish_line(&self) {
        let mut state = self.state.lock().expect("progress state poisoned");
        if matches!(self.mode, ProgressRenderMode::Plain { interactive: true })
            && state.last_line_len > 0
        {
            eprintln!();
            state.last_line_len = 0;
        }
    }

    pub(crate) fn warning(&self, message: impl AsRef<str>) {
        if matches!(self.mode, ProgressRenderMode::None) {
            return;
        }
        self.finish_line();
        match self.mode {
            ProgressRenderMode::Json => {
                eprintln!(
                    "{}",
                    json!({
                        "type": "ctx_progress",
                        "operation": self.operation,
                        "level": "warning",
                        "message": message.as_ref(),
                    })
                );
            }
            ProgressRenderMode::Plain { .. } => eprintln!("warning: {}", message.as_ref()),
            ProgressRenderMode::None => {}
        }
    }

    pub(crate) fn codex_import_callback(
        &self,
        source: &SourceInfo,
        source_offset_bytes: u64,
    ) -> Option<CodexSessionImportProgressCallback> {
        if !self.is_enabled() || source.provider != CaptureProvider::Codex {
            return None;
        }
        let reporter = self.clone();
        let provider = source.provider.as_str().to_owned();
        Some(Arc::new(move |progress: CodexSessionImportProgress| {
            let completed_bytes = source_offset_bytes.saturating_add(progress.completed_bytes);
            reporter.emit(ProgressLine {
                phase: "indexing",
                message: provider.clone(),
                completed_bytes,
                total_bytes: reporter.total_bytes.max(completed_bytes),
                completed_files: Some(progress.completed_files),
                total_files: Some(progress.total_files),
                imported_events: Some(progress.imported_events),
                done: progress.done,
                force: progress.done,
            });
        }))
    }

    pub(crate) fn parallel_codex_import_callback(
        &self,
        source: &SourceInfo,
        source_index: usize,
        source_states: Arc<Mutex<Vec<SourceProgressSnapshot>>>,
    ) -> Option<CodexSessionImportProgressCallback> {
        if !self.is_enabled() || source.provider != CaptureProvider::Codex {
            return None;
        }
        let reporter = self.clone();
        let provider = source.provider.as_str().to_owned();
        Some(Arc::new(move |progress: CodexSessionImportProgress| {
            let (completed_bytes, total_bytes) = {
                let mut states = source_states
                    .lock()
                    .expect("parallel progress state poisoned");
                if let Some(state) = states.get_mut(source_index) {
                    state.total_bytes = state.total_bytes.max(progress.total_bytes);
                    state.completed_bytes = progress
                        .completed_bytes
                        .min(state.total_bytes.max(progress.completed_bytes));
                }
                aggregate_source_progress(&states)
            };
            reporter.emit(ProgressLine {
                phase: "indexing",
                message: provider.clone(),
                completed_bytes,
                total_bytes: reporter.total_bytes.max(total_bytes).max(completed_bytes),
                completed_files: Some(progress.completed_files),
                total_files: Some(progress.total_files),
                imported_events: Some(progress.imported_events),
                done: progress.done,
                force: progress.done,
            });
        }))
    }

    pub(crate) fn parallel_source_done(
        &self,
        source: &SourceInfo,
        source_index: usize,
        source_states: &Arc<Mutex<Vec<SourceProgressSnapshot>>>,
        stats: SourceStats,
        summary: &ProviderImportSummary,
    ) {
        if !self.is_enabled() {
            return;
        }
        let (completed_bytes, total_bytes) = {
            let mut states = source_states
                .lock()
                .expect("parallel progress state poisoned");
            if let Some(state) = states.get_mut(source_index) {
                state.total_bytes = state.total_bytes.max(stats.bytes);
                state.completed_bytes = state.total_bytes;
            }
            aggregate_source_progress(&states)
        };
        self.emit(ProgressLine {
            phase: "indexing",
            message: format!("imported {}", source.provider.as_str()),
            completed_bytes,
            total_bytes: self.total_bytes.max(total_bytes).max(completed_bytes),
            completed_files: Some(stats.files),
            total_files: Some(stats.files),
            imported_events: Some(summary.imported_events),
            done: true,
            force: true,
        });
    }

    pub(crate) fn parallel_source_failed(
        &self,
        source: &SourceInfo,
        source_index: usize,
        source_states: &Arc<Mutex<Vec<SourceProgressSnapshot>>>,
        stats: SourceStats,
        error: &str,
    ) {
        if !self.is_enabled() {
            return;
        }
        let (completed_bytes, total_bytes) = {
            let mut states = source_states
                .lock()
                .expect("parallel progress state poisoned");
            if let Some(state) = states.get_mut(source_index) {
                state.total_bytes = state.total_bytes.max(stats.bytes);
                state.completed_bytes = state.total_bytes;
            }
            aggregate_source_progress(&states)
        };
        self.emit(ProgressLine {
            phase: "indexing",
            message: format!(
                "skipped {}: {}",
                source.provider.as_str(),
                source_error_reason(source, error)
            ),
            completed_bytes,
            total_bytes: self.total_bytes.max(total_bytes).max(completed_bytes),
            completed_files: Some(stats.files),
            total_files: Some(stats.files),
            imported_events: Some(0),
            done: true,
            force: true,
        });
    }

    fn emit(&self, line: ProgressLine) {
        let mut state = self.state.lock().expect("progress state poisoned");
        let now = Instant::now();
        if !line.force
            && state
                .last_emit
                .is_some_and(|last| now.duration_since(last) < StdDuration::from_millis(900))
        {
            return;
        }
        state.last_emit = Some(now);
        let elapsed = now.duration_since(state.started);
        match self.mode {
            ProgressRenderMode::None => {}
            ProgressRenderMode::Json => {
                let value = json!({
                    "type": "ctx_progress",
                    "operation": self.operation,
                    "phase": line.phase,
                    "message": line.message,
                    "completed_bytes": line.completed_bytes,
                    "total_bytes": line.total_bytes,
                    "percent": progress_percent(line.completed_bytes, line.total_bytes),
                    "elapsed_seconds": elapsed.as_secs_f64(),
                    "eta_seconds": eta_seconds(line.completed_bytes, line.total_bytes, elapsed),
                    "completed_files": line.completed_files,
                    "total_files": line.total_files,
                    "imported_events": line.imported_events,
                    "done": line.done,
                });
                eprintln!("{value}");
            }
            ProgressRenderMode::Plain { interactive } => {
                let rendered = render_progress_line(&line, elapsed);
                if interactive {
                    let padding = state.last_line_len.saturating_sub(rendered.len());
                    eprint!("\r{}{}", rendered, " ".repeat(padding));
                    if line.done {
                        eprintln!();
                        state.last_line_len = 0;
                    } else {
                        state.last_line_len = rendered.len();
                        let _ = std::io::stderr().flush();
                    }
                } else {
                    eprintln!("{rendered}");
                }
            }
        }
    }
}

fn aggregate_source_progress(states: &[SourceProgressSnapshot]) -> (u64, u64) {
    states
        .iter()
        .fold((0u64, 0u64), |(completed, total), state| {
            let source_total = state.total_bytes.max(state.completed_bytes);
            (
                completed.saturating_add(state.completed_bytes.min(source_total)),
                total.saturating_add(source_total),
            )
        })
}

struct ProgressLine {
    phase: &'static str,
    message: String,
    completed_bytes: u64,
    total_bytes: u64,
    completed_files: Option<usize>,
    total_files: Option<usize>,
    imported_events: Option<usize>,
    done: bool,
    force: bool,
}

fn render_progress_line(line: &ProgressLine, elapsed: StdDuration) -> String {
    let percent = progress_percent(line.completed_bytes, line.total_bytes);
    let bar = progress_bar(percent, 20);
    let eta = eta_seconds(line.completed_bytes, line.total_bytes, elapsed)
        .map(format_seconds)
        .unwrap_or_else(|| "estimating".to_owned());
    let files = match (line.completed_files, line.total_files) {
        (Some(done), Some(total)) if total > 0 => format!(" {done}/{total} files"),
        _ => String::new(),
    };
    let events = line
        .imported_events
        .map(|events| format!(" {events} events"))
        .unwrap_or_default();
    let remaining = if line.done {
        "done".to_owned()
    } else {
        format!("{eta} left")
    };
    format!(
        "{:<10} [{}] {:>5.1}% {}/{}{}{} {} - {}",
        line.phase,
        bar,
        percent,
        format_bytes(line.completed_bytes),
        format_bytes(line.total_bytes),
        files,
        events,
        remaining,
        line.message
    )
}

fn progress_percent(completed: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

fn eta_seconds(completed: u64, total: u64, elapsed: StdDuration) -> Option<f64> {
    if completed == 0 || total <= completed {
        return None;
    }
    let rate = completed as f64 / elapsed.as_secs_f64().max(0.001);
    if rate <= 0.0 {
        return None;
    }
    Some((total - completed) as f64 / rate)
}

fn progress_bar(percent: f64, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f64).round() as usize;
    format!(
        "{}{}",
        "#".repeat(filled.min(width)),
        "-".repeat(width.saturating_sub(filled))
    )
}

fn format_seconds(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        let minutes = seconds / 60;
        let rem = seconds % 60;
        format!("{minutes}m{rem:02}s")
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}
pub(crate) fn progress_mode_name(progress: ProgressArg) -> &'static str {
    match progress {
        ProgressArg::Auto => "auto",
        ProgressArg::Plain => "plain",
        ProgressArg::Json => "json",
        ProgressArg::None => "none",
    }
}

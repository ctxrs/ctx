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
        self.emit_status(ProgressLine {
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
        self.emit_status(ProgressLine {
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

    pub(crate) fn notice(&self, message: impl AsRef<str>) {
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
                        "level": "info",
                        "message": message.as_ref(),
                    })
                );
            }
            ProgressRenderMode::Plain { .. } => eprintln!("{}\n", message.as_ref()),
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
                eprintln!("{}", progress_json(self.operation, &line, elapsed));
            }
            ProgressRenderMode::Plain { interactive } => {
                let rendered = render_progress_line(&line, elapsed);
                if interactive {
                    eprint!("\r\u{1b}[2K{rendered}");
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

    fn emit_status(&self, line: ProgressLine) {
        match self.mode {
            ProgressRenderMode::None => {}
            ProgressRenderMode::Json => {
                let elapsed = self
                    .state
                    .lock()
                    .expect("progress state poisoned")
                    .started
                    .elapsed();
                eprintln!("{}", progress_json(self.operation, &line, elapsed));
            }
            ProgressRenderMode::Plain { .. } => {
                self.finish_line();
                eprintln!("{}", line.message);
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
    render_progress_line_for_width(line, elapsed, progress_render_width())
}

fn render_progress_line_for_width(
    line: &ProgressLine,
    elapsed: StdDuration,
    target_width: usize,
) -> String {
    let (completed_bytes, total_bytes) = progress_line_bytes(line);
    let percent = progress_line_percent(line);
    let phase = progress_phase_label(line.phase);
    let bar = progress_bar(percent, 10);
    let bytes = format_byte_range(completed_bytes, total_bytes);
    let files = line
        .completed_files
        .filter(|_| !line.done)
        .map(|done| format!(" {} files", format_count(done)))
        .unwrap_or_default();
    let remaining = if line.done {
        "done".to_owned()
    } else if let Some(eta) = progress_line_eta_seconds(line, elapsed) {
        format_eta_compact(eta)
    } else {
        "working".to_owned()
    };
    let target_width = target_width.clamp(36, 76);
    let candidates = [
        format!("{phase} [{bar}] {percent:>3.0}%  {bytes}{files}  {remaining}"),
        format!("{phase} [{bar}] {percent:>3.0}%  {bytes}  {remaining}"),
        format!("{phase} [{bar}] {percent:>3.0}%  {remaining}"),
    ];
    candidates
        .into_iter()
        .find(|line| line.chars().count() <= target_width)
        .map(|line| truncate_progress_line(&line, target_width))
        .unwrap_or_else(|| {
            truncate_progress_line(
                &format!("{phase} {percent:>3.0}% {remaining}"),
                target_width,
            )
        })
}

fn progress_json(operation: &'static str, line: &ProgressLine, elapsed: StdDuration) -> String {
    let (completed_bytes, total_bytes) = progress_line_bytes(line);
    json!({
        "type": "ctx_progress",
        "operation": operation,
        "phase": line.phase,
        "message": line.message,
        "completed_bytes": completed_bytes,
        "total_bytes": total_bytes,
        "percent": progress_line_percent(line),
        "elapsed_seconds": elapsed.as_secs_f64(),
        "eta_seconds": progress_line_eta_seconds(line, elapsed),
        "completed_files": line.completed_files,
        "total_files": line.total_files,
        "imported_events": line.imported_events,
        "done": line.done,
    })
    .to_string()
}

fn progress_render_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 40)
        .unwrap_or(80)
        .saturating_sub(1)
        .min(76)
}

fn progress_phase_label(phase: &str) -> String {
    let mut chars = phase.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Working".to_owned(),
    }
}

fn truncate_progress_line(line: &str, target_width: usize) -> String {
    if line.chars().count() <= target_width {
        return line.to_owned();
    }
    let keep = target_width.saturating_sub(1);
    let mut truncated = line.chars().take(keep).collect::<String>();
    truncated.push('…');
    truncated
}

fn progress_percent(completed: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

fn progress_line_bytes(line: &ProgressLine) -> (u64, u64) {
    let total_bytes = line.total_bytes.max(line.completed_bytes);
    let completed_bytes = if line.done {
        total_bytes
    } else {
        line.completed_bytes.min(total_bytes)
    };
    (completed_bytes, total_bytes)
}

fn progress_line_percent(line: &ProgressLine) -> f64 {
    if line.done {
        100.0
    } else {
        let (completed_bytes, total_bytes) = progress_line_bytes(line);
        progress_percent(completed_bytes, total_bytes)
    }
}

fn progress_line_eta_seconds(line: &ProgressLine, elapsed: StdDuration) -> Option<f64> {
    if line.done {
        None
    } else {
        let (completed_bytes, total_bytes) = progress_line_bytes(line);
        eta_seconds(completed_bytes, total_bytes, elapsed)
    }
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

fn format_eta_compact(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        format!("~{}s", seconds.max(1))
    } else if seconds < 3600 {
        format!("~{}m", (seconds + 30) / 60)
    } else {
        format!("~{}h", (seconds + 1800) / 3600)
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    let (value, unit) = scaled_bytes(bytes);
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn format_byte_range(completed: u64, total: u64) -> String {
    let (_, total_unit) = scaled_bytes(total);
    if total_unit == "B" {
        return format!("{completed}/{total} B");
    }
    let completed_value = bytes_in_unit(completed, total_unit);
    let total_value = bytes_in_unit(total, total_unit);
    format!("{completed_value:.1}/{total_value:.1} {total_unit}")
}

fn scaled_bytes(bytes: u64) -> (f64, &'static str) {
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < BYTE_UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    (value, BYTE_UNITS[unit])
}

fn bytes_in_unit(bytes: u64, target_unit: &str) -> f64 {
    let mut value = bytes as f64;
    let target_index = BYTE_UNITS
        .iter()
        .position(|unit| *unit == target_unit)
        .unwrap_or(0);
    for _ in 0..target_index {
        value /= 1024.0;
    }
    value
}

const BYTE_UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

pub(crate) fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first_group_len = digits.len() % 3;
    for (index, ch) in digits.chars().enumerate() {
        if index > 0
            && (index == first_group_len
                || (index > first_group_len && (index - first_group_len).is_multiple_of(3)))
        {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn plural(value: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if value == 1 {
        singular
    } else {
        plural
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_line() -> ProgressLine {
        ProgressLine {
            phase: "indexing",
            message: "codex".to_owned(),
            completed_bytes: 7 * 1024 * 1024 * 1024,
            total_bytes: 14 * 1024 * 1024 * 1024,
            completed_files: Some(8_420),
            total_files: Some(32_581),
            imported_events: Some(418_204),
            done: false,
            force: false,
        }
    }

    #[test]
    fn compact_progress_line_omits_trailing_message_and_events() {
        let rendered =
            render_progress_line_for_width(&sample_line(), StdDuration::from_secs(120), 76);

        assert!(rendered.chars().count() <= 76, "{rendered}");
        assert!(rendered.starts_with("Indexing ["), "{rendered}");
        assert!(rendered.contains("7.0/14.0 GiB"), "{rendered}");
        assert!(rendered.contains("8,420 files"), "{rendered}");
        assert!(!rendered.contains("codex"), "{rendered}");
        assert!(!rendered.contains("events"), "{rendered}");
    }

    #[test]
    fn compact_progress_line_drops_files_on_narrow_widths() {
        let rendered =
            render_progress_line_for_width(&sample_line(), StdDuration::from_secs(120), 45);

        assert!(rendered.chars().count() <= 45, "{rendered}");
        assert!(rendered.starts_with("Indexing ["), "{rendered}");
        assert!(!rendered.contains("files"), "{rendered}");
    }

    #[test]
    fn done_progress_line_forces_complete_percent_with_incomplete_bytes() {
        let mut line = sample_line();
        line.phase = "finalizing";
        line.completed_bytes = 0;
        line.total_bytes = 4 * 1024;
        line.done = true;

        let rendered = render_progress_line_for_width(&line, StdDuration::from_secs(120), 76);

        assert!(rendered.contains("[##########]"), "{rendered}");
        assert!(rendered.contains("100%"), "{rendered}");
        assert!(rendered.contains("4.0/4.0 KiB"), "{rendered}");
        assert!(rendered.ends_with("done"), "{rendered}");
        assert!(!rendered.contains("  0%"), "{rendered}");
    }

    #[test]
    fn done_progress_json_forces_complete_bytes_with_incomplete_bytes() {
        let mut line = sample_line();
        line.phase = "finalizing";
        line.completed_bytes = 0;
        line.total_bytes = 4 * 1024;
        line.done = true;

        let value: serde_json::Value =
            serde_json::from_str(&progress_json("setup", &line, StdDuration::from_secs(120)))
                .expect("progress json should parse");

        assert_eq!(value["completed_bytes"], 4 * 1024);
        assert_eq!(value["total_bytes"], 4 * 1024);
        assert_eq!(value["percent"], 100.0);
        assert_eq!(value["eta_seconds"], serde_json::Value::Null);
        assert_eq!(value["done"], true);
    }
}

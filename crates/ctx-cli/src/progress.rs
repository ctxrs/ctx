use std::{
    io::IsTerminal,
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use clap::ValueEnum;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProgressArg {
    Auto,
    Plain,
    Json,
    None,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressRenderMode {
    None,
    Plain,
    Json,
}

#[derive(Debug)]
struct ProgressState {
    started: Instant,
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
            ProgressArg::Plain => ProgressRenderMode::Plain,
            ProgressArg::Auto if json_output || !stderr_is_terminal => ProgressRenderMode::None,
            ProgressArg::Auto => ProgressRenderMode::Plain,
        };
        Self {
            mode,
            operation,
            total_bytes,
            state: Arc::new(Mutex::new(ProgressState {
                started: Instant::now(),
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
        });
    }

    pub(crate) fn finish_line(&self) {}

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
            ProgressRenderMode::Plain => eprintln!("{}", line.message),
        }
    }
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

pub(crate) fn format_bytes(bytes: u64) -> String {
    let (value, unit) = scaled_bytes(bytes);
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {unit}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_progress_json_forces_complete_bytes_with_incomplete_bytes() {
        let line = ProgressLine {
            phase: "finalizing",
            message: "done".to_owned(),
            completed_bytes: 0,
            total_bytes: 4 * 1024,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: true,
        };

        let value: serde_json::Value =
            serde_json::from_str(&progress_json("setup", &line, StdDuration::from_secs(120)))
                .expect("progress json should parse");

        assert_eq!(value["completed_bytes"], 4 * 1024);
        assert_eq!(value["total_bytes"], 4 * 1024);
        assert_eq!(value["percent"], 100.0);
        assert_eq!(value["eta_seconds"], serde_json::Value::Null);
        assert_eq!(value["done"], true);
    }

    #[test]
    fn progress_json_remains_exact_and_ansi_free() {
        let line = ProgressLine {
            phase: "cataloging",
            message: "cataloging".to_owned(),
            completed_bytes: 1024,
            total_bytes: 4096,
            completed_files: Some(1),
            total_files: Some(2),
            imported_events: Some(7),
            done: false,
        };

        let rendered = progress_json("import", &line, StdDuration::from_secs(2));

        assert_eq!(
            rendered,
            concat!(
                r#"{"completed_bytes":1024,"completed_files":1,"done":false,"#,
                r#""elapsed_seconds":2.0,"eta_seconds":6.0,"imported_events":7,"#,
                r#""message":"cataloging","operation":"import","percent":25.0,"#,
                r#""phase":"cataloging","total_bytes":4096,"total_files":2,"#,
                r#""type":"ctx_progress"}"#,
            )
        );
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn plain_and_json_progress_keep_explicit_stream_contracts() {
        let line = ProgressLine {
            phase: "indexing",
            message: "Indexed 2 sources".to_owned(),
            completed_bytes: 2,
            total_bytes: 4,
            completed_files: Some(2),
            total_files: Some(4),
            imported_events: None,
            done: false,
        };

        let plain = match ProgressRenderMode::Plain {
            ProgressRenderMode::Plain => line.message.as_str(),
            _ => unreachable!(),
        };
        let json = match ProgressRenderMode::Json {
            ProgressRenderMode::Json => progress_json("import", &line, StdDuration::from_secs(1)),
            _ => unreachable!(),
        };

        assert_eq!(plain, "Indexed 2 sources");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["type"],
            "ctx_progress"
        );
        assert!(!plain.contains('\u{1b}'));
        assert!(!json.contains('\u{1b}'));
    }
}

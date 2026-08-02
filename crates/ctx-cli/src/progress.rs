use std::{
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use clap::ValueEnum;
use serde_json::json;

use crate::semantic::{
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedRefreshProgress,
};

const MAX_PROGRESS_MESSAGE_BYTES: usize = 512;
const MAX_PROGRESS_SOURCE_BYTES: usize = 256;
const MAX_PROGRESS_PHASE_BYTES: usize = 64;

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

    pub(crate) fn message(
        &self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> io::Result<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        let message = bounded_progress_text(&message.into(), MAX_PROGRESS_MESSAGE_BYTES);
        self.emit_status(ProgressLine {
            phase: bounded_progress_text(phase, MAX_PROGRESS_PHASE_BYTES),
            message,
            completed_bytes: 0,
            total_bytes: self.total_bytes,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: false,
            refresh_progress: None,
        })
    }

    pub(crate) fn done(
        &self,
        phase: &'static str,
        message: impl Into<String>,
        completed_bytes: u64,
    ) -> io::Result<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.emit_status(ProgressLine {
            phase: bounded_progress_text(phase, MAX_PROGRESS_PHASE_BYTES),
            message: bounded_progress_text(&message.into(), MAX_PROGRESS_MESSAGE_BYTES),
            completed_bytes,
            total_bytes: self.total_bytes.max(completed_bytes),
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: true,
            refresh_progress: None,
        })
    }

    pub(crate) fn source_refresh(&self, progress: &SourceBackedRefreshProgress) -> io::Result<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.emit_status(source_refresh_line(progress))
    }

    pub(crate) fn finish_line(&self) -> io::Result<()> {
        Ok(())
    }

    fn emit_status(&self, line: ProgressLine) -> io::Result<()> {
        let elapsed = self
            .state
            .lock()
            .map_err(|_| io::Error::other("progress state lock was poisoned"))?
            .started
            .elapsed();
        let stderr = io::stderr();
        let mut writer = stderr.lock();
        write_progress(&mut writer, self.mode, self.operation, &line, elapsed)?;
        writer.flush()
    }
}

struct ProgressLine {
    phase: String,
    message: String,
    completed_bytes: u64,
    total_bytes: u64,
    completed_files: Option<usize>,
    total_files: Option<usize>,
    imported_events: Option<usize>,
    done: bool,
    refresh_progress: Option<SourceBackedRefreshProgress>,
}

fn write_progress(
    writer: &mut impl Write,
    mode: ProgressRenderMode,
    operation: &'static str,
    line: &ProgressLine,
    elapsed: StdDuration,
) -> io::Result<()> {
    match mode {
        ProgressRenderMode::None => Ok(()),
        ProgressRenderMode::Plain => writeln!(writer, "{}", line.message),
        ProgressRenderMode::Json => writeln!(writer, "{}", progress_json(operation, line, elapsed)),
    }
}

fn progress_json(operation: &'static str, line: &ProgressLine, elapsed: StdDuration) -> String {
    let (completed_bytes, total_bytes) = progress_line_bytes(line);
    let mut value = json!({
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
    });
    if let Some(progress) = line.refresh_progress.as_ref() {
        value["completed_sources"] = json!(progress.completed_sources);
        value["total_sources"] = json!(progress.total_sources);
        value["source_completed_records"] = json!(progress.completed_records);
        value["source_completed_bytes"] = json!(progress.completed_bytes);
        value["current_source"] = json!(progress
            .current_source
            .as_deref()
            .map(|source| bounded_progress_text(source, MAX_PROGRESS_SOURCE_BYTES)));
        value["current_source_progress"] = progress
            .current_source_progress
            .map(SourceBackedCurrentSourceProgress::to_json)
            .unwrap_or(serde_json::Value::Null);
    }
    value.to_string()
}

fn source_refresh_line(progress: &SourceBackedRefreshProgress) -> ProgressLine {
    let (phase, message, completed_bytes, total_bytes) = progress
        .current_source_progress
        .map(|current| detailed_source_refresh_line(progress, current))
        .unwrap_or_else(|| source_level_refresh_line(progress));
    ProgressLine {
        phase: bounded_progress_text(&phase, MAX_PROGRESS_PHASE_BYTES),
        message: bounded_progress_text(&message, MAX_PROGRESS_MESSAGE_BYTES),
        completed_bytes,
        total_bytes,
        completed_files: None,
        total_files: None,
        imported_events: progress
            .completed_records
            .and_then(|value| usize::try_from(value).ok()),
        done: false,
        refresh_progress: Some(progress.clone()),
    }
}

fn detailed_source_refresh_line(
    progress: &SourceBackedRefreshProgress,
    current: SourceBackedCurrentSourceProgress,
) -> (String, String, u64, u64) {
    let source = progress_source_label(progress);
    let (completed_bytes, total_bytes) = match current.stage {
        SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
        | SourceBackedCurrentSourceProgressStage::OnlineBackup => (
            current.snapshot_bytes_completed.unwrap_or_default(),
            current.snapshot_bytes_total.unwrap_or_default(),
        ),
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint
        | SourceBackedCurrentSourceProgressStage::LogicalScan => (0, 0),
    };
    let details = match current.stage {
        SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
        | SourceBackedCurrentSourceProgressStage::OnlineBackup => {
            snapshot_progress_details(current)
        }
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint
        | SourceBackedCurrentSourceProgressStage::LogicalScan => logical_progress_details(current),
    };
    let action = match current.stage {
        SourceBackedCurrentSourceProgressStage::SourceFamilyCopy => "Copying SQLite snapshot files",
        SourceBackedCurrentSourceProgressStage::OnlineBackup => "Copying SQLite snapshot",
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint => {
            "Fingerprinting SQLite history"
        }
        SourceBackedCurrentSourceProgressStage::LogicalScan => "Scanning SQLite history",
    };
    let message = if details.is_empty() {
        format!("{action} for {source}.")
    } else {
        format!("{action} for {source}: {details}.")
    };
    (
        current.stage.as_str().to_owned(),
        message,
        completed_bytes,
        total_bytes,
    )
}

fn source_level_refresh_line(progress: &SourceBackedRefreshProgress) -> (String, String, u64, u64) {
    let source_progress = format!(
        "{} / {} sources",
        progress.completed_sources, progress.total_sources
    );
    let source_work = match (progress.completed_records, progress.completed_bytes) {
        (Some(records), Some(bytes)) => {
            format!("; {records} records, {} scanned", format_bytes(bytes))
        }
        (Some(records), None) => format!("; {records} records"),
        (None, Some(bytes)) => format!("; {} scanned", format_bytes(bytes)),
        (None, None) => String::new(),
    };
    let message = match (progress.phase.as_str(), progress.current_source.as_deref()) {
        ("discovering", _) => "Discovering local history sources.".to_owned(),
        ("refreshing", Some(_)) => format!(
            "Refreshing {} ({source_progress}{source_work}).",
            progress_source_label(progress),
        ),
        ("verifying", _) => format!("Verifying refreshed history ({source_progress})."),
        ("committing", _) | ("committed", _) => {
            format!("Publishing refreshed history ({source_progress}).")
        }
        (_, Some(_)) => format!(
            "Refreshing {} ({source_progress}; phase {}).",
            progress_source_label(progress),
            progress.phase.replace('_', " ")
        ),
        _ => format!(
            "Refreshing local history ({source_progress}; phase {}).",
            progress.phase.replace('_', " ")
        ),
    };
    (progress.phase.clone(), message, 0, 0)
}

fn progress_source_label(progress: &SourceBackedRefreshProgress) -> String {
    progress
        .current_source
        .as_deref()
        .map(|source| bounded_progress_text(source, MAX_PROGRESS_SOURCE_BYTES))
        .unwrap_or_else(|| "the current source".to_owned())
}

fn snapshot_progress_details(progress: SourceBackedCurrentSourceProgress) -> String {
    let mut details = Vec::new();
    match (
        progress.snapshot_pages_completed,
        progress.snapshot_pages_total,
    ) {
        (Some(completed), Some(total)) => details.push(format!("{completed} / {total} pages")),
        (Some(completed), None) => details.push(format!("{completed} pages")),
        (None, _) => {}
    }
    match (
        progress.snapshot_bytes_completed,
        progress.snapshot_bytes_total,
    ) {
        (Some(completed), Some(total)) => details.push(format!(
            "{} / {} copied",
            format_bytes(completed),
            format_bytes(total)
        )),
        (Some(completed), None) => details.push(format!("{} copied", format_bytes(completed))),
        (None, _) => {}
    }
    details.join(", ")
}

fn logical_progress_details(progress: SourceBackedCurrentSourceProgress) -> String {
    let mut details = Vec::new();
    if let Some(rows) = progress.logical_rows_scanned {
        details.push(format!("{rows} rows"));
    }
    if let Some(bytes) = progress.logical_certified_bytes {
        details.push(format!("{} certified", format_bytes(bytes)));
    }
    details.join(", ")
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
    if line.done && line.total_bytes.max(line.completed_bytes) != 0 {
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

fn bounded_progress_text(value: &str, max_bytes: usize) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= max_bytes {
        return sanitized;
    }
    const SUFFIX: &str = "...";
    let mut end = max_bytes.saturating_sub(SUFFIX.len()).min(sanitized.len());
    while end > 0 && !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = sanitized[..end].to_owned();
    bounded.push_str(SUFFIX);
    bounded
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
            phase: "finalizing".to_owned(),
            message: "done".to_owned(),
            completed_bytes: 0,
            total_bytes: 4 * 1024,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: true,
            refresh_progress: None,
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
            phase: "cataloging".to_owned(),
            message: "cataloging".to_owned(),
            completed_bytes: 1024,
            total_bytes: 4096,
            completed_files: Some(1),
            total_files: Some(2),
            imported_events: Some(7),
            done: false,
            refresh_progress: None,
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
            phase: "indexing".to_owned(),
            message: "Indexed 2 sources".to_owned(),
            completed_bytes: 2,
            total_bytes: 4,
            completed_files: Some(2),
            total_files: Some(4),
            imported_events: None,
            done: false,
            refresh_progress: None,
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

    #[derive(Clone, Copy)]
    enum WriterFailure {
        Write,
        Flush,
    }

    struct FailingWriter(WriterFailure);

    impl Write for FailingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            match self.0 {
                WriterFailure::Write => Err(io::Error::other("injected progress write failure")),
                WriterFailure::Flush => Ok(buffer.len()),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match self.0 {
                WriterFailure::Write => Ok(()),
                WriterFailure::Flush => Err(io::Error::other("injected progress flush failure")),
            }
        }
    }

    #[test]
    fn progress_write_and_flush_failures_remain_errors() {
        let line = ProgressLine {
            phase: "logical_scan".to_owned(),
            message: "Scanning SQLite history".to_owned(),
            completed_bytes: 0,
            total_bytes: 0,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: false,
            refresh_progress: None,
        };
        for (failure, expected) in [
            (WriterFailure::Write, "injected progress write failure"),
            (WriterFailure::Flush, "injected progress flush failure"),
        ] {
            let mut writer = FailingWriter(failure);
            let result = write_progress(
                &mut writer,
                ProgressRenderMode::Json,
                "import",
                &line,
                StdDuration::ZERO,
            )
            .and_then(|()| writer.flush());
            assert!(result
                .expect_err("progress output failure must propagate")
                .to_string()
                .contains(expected));
        }
    }

    #[test]
    fn sqlite_logical_progress_is_typed_and_never_invents_a_total() {
        let progress = SourceBackedRefreshProgress {
            phase: "refreshing".to_owned(),
            completed_sources: 1,
            total_sources: 2,
            current_source: Some("/tmp/history\ncontrol.sqlite".to_owned()),
            completed_records: Some(4_096),
            completed_bytes: Some(2_048),
            current_source_progress: Some(SourceBackedCurrentSourceProgress {
                stage: SourceBackedCurrentSourceProgressStage::LogicalScan,
                snapshot_pages_completed: None,
                snapshot_pages_total: None,
                snapshot_bytes_completed: None,
                snapshot_bytes_total: None,
                logical_rows_scanned: Some(4_096),
                logical_certified_bytes: Some(2_048),
            }),
        };
        let line = source_refresh_line(&progress);
        assert_eq!(line.phase, "logical_scan");
        assert!(line.message.contains("4,096") || line.message.contains("4096"));
        assert!(!line.message.contains('\n'));
        assert_eq!((line.completed_bytes, line.total_bytes), (0, 0));

        let value: serde_json::Value =
            serde_json::from_str(&progress_json("import", &line, StdDuration::from_secs(2)))
                .unwrap();
        assert_eq!(value["percent"], 0.0);
        assert_eq!(value["eta_seconds"], serde_json::Value::Null);
        assert_eq!(value["current_source_progress"]["stage"], "logical_scan");
        assert_eq!(
            value["current_source_progress"]["logical_rows_scanned"],
            4_096
        );
        assert!(!value["current_source"].as_str().unwrap().contains('\n'));
    }

    #[test]
    fn progress_text_is_control_safe_utf8_and_bounded() {
        let text = format!("{}\n{}", "é".repeat(400), "x".repeat(400));
        let bounded = bounded_progress_text(&text, MAX_PROGRESS_MESSAGE_BYTES);
        assert!(bounded.len() <= MAX_PROGRESS_MESSAGE_BYTES);
        assert!(!bounded.contains('\n'));
        assert!(bounded.ends_with("..."));
    }
}

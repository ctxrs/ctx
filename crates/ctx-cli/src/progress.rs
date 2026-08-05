use std::{
    fmt,
    io::{self, Write},
    time::{Duration as StdDuration, Instant},
};

use clap::ValueEnum;
use serde_json::json;

use crate::{
    semantic::{RefreshStatus, SourceBackedCurrentSourceProgress},
    ui::{refresh_progress, Document, Line, LiveOutput, RefreshProgressSnapshot, Span, Token, Ui},
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
    Live,
    Plain,
    Json,
}

#[derive(Debug)]
pub(crate) struct ProgressWriterError(io::Error);

impl fmt::Display for ProgressWriterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "write progress output: {}", self.0)
    }
}

impl std::error::Error for ProgressWriterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub(crate) struct ProgressReporter<'a> {
    mode: ProgressRenderMode,
    operation: &'static str,
    total_bytes: u64,
    started: Instant,
    output: LiveOutput<&'a mut (dyn Write + Send)>,
}

impl<'a> ProgressReporter<'a> {
    pub(crate) fn new(
        ui: &'a mut Ui,
        arg: ProgressArg,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        let live_output_capable = ui.stderr_context().live_output_capable();
        let mode = match arg {
            ProgressArg::None => ProgressRenderMode::None,
            ProgressArg::Json => ProgressRenderMode::Json,
            ProgressArg::Plain => ProgressRenderMode::Plain,
            ProgressArg::Auto if json_output || !live_output_capable => ProgressRenderMode::None,
            ProgressArg::Auto => ProgressRenderMode::Live,
        };
        Self {
            mode,
            operation,
            total_bytes,
            started: Instant::now(),
            output: ui.stderr_live_output(),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.mode != ProgressRenderMode::None
    }

    pub(crate) fn message(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
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
            refresh: None,
        })
    }

    pub(crate) fn source_refresh(
        &mut self,
        status: &RefreshStatus,
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() {
            return Ok(());
        }
        let snapshot = RefreshProgressSnapshot::from_status(status).map_err(|error| {
            ProgressWriterError(io::Error::new(io::ErrorKind::InvalidData, error))
        })?;
        self.emit_status(source_refresh_line(snapshot, self.total_bytes))
    }

    fn emit_status(&mut self, line: ProgressLine) -> Result<(), ProgressWriterError> {
        let elapsed = self.started.elapsed();
        write_progress(&mut self.output, self.mode, self.operation, &line, elapsed)
            .map_err(ProgressWriterError)
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
    refresh: Option<RefreshProgressSnapshot>,
}

fn write_progress<W: Write>(
    output: &mut LiveOutput<W>,
    mode: ProgressRenderMode,
    operation: &'static str,
    line: &ProgressLine,
    elapsed: StdDuration,
) -> io::Result<()> {
    match mode {
        ProgressRenderMode::None => Ok(()),
        ProgressRenderMode::Live => {
            let document = line.refresh.as_ref().map_or_else(
                || Document::from_line(Line::new().with(Span::new(&line.message, Token::Text))),
                |snapshot| refresh_progress(output.context(), snapshot),
            );
            output.write_frame(&document, line.done)
        }
        ProgressRenderMode::Plain => output.write_line(&line.message),
        ProgressRenderMode::Json => output.write_line(&progress_json(operation, line, elapsed)),
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
    if let Some(snapshot) = line.refresh.as_ref() {
        let progress = snapshot.progress();
        value["completed_sources"] = json!(progress.completed_sources);
        value["total_sources"] = json!(progress.total_sources);
        value["total_sources_known"] = json!(snapshot.total_sources_known());
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
        let status = snapshot.status().schema_v1_fields();
        for field in [
            "request_id",
            "request_state",
            "logical_request_id",
            "logical_phase",
            "physical_attempt_id",
            "physical_attempt_state",
            "progress_owner_request_id",
            "progress_owner_attempt_state",
            "structured_outcome",
            "maintenance_wake",
        ] {
            if let Some(field_value) = status.get(field) {
                value[field] = field_value.clone();
            }
        }
    }
    value.to_string()
}

fn source_refresh_line(snapshot: RefreshProgressSnapshot, total_bytes: u64) -> ProgressLine {
    let (completed_bytes, current_total_bytes) = snapshot.byte_progress();
    let phase = snapshot.phase();
    let message = snapshot.message();
    let done = snapshot.is_terminal();
    let imported_events = snapshot
        .progress()
        .completed_records
        .and_then(|value| usize::try_from(value).ok());
    ProgressLine {
        phase: bounded_progress_text(&phase, MAX_PROGRESS_PHASE_BYTES),
        message: bounded_progress_text(&message, MAX_PROGRESS_MESSAGE_BYTES),
        completed_bytes,
        total_bytes: current_total_bytes.max(total_bytes),
        completed_files: None,
        total_files: None,
        imported_events,
        done,
        refresh: Some(snapshot),
    }
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
    use std::sync::{Arc, Mutex};

    use super::*;
    use serde_json::json;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn active_status() -> RefreshStatus {
        RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "running",
            "logical_request_id": "logical-request",
            "logical_phase": "direct",
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": "running",
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": "running",
            "progress": {
                "phase": "refreshing",
                "completed_sources": 1,
                "total_sources": 2,
                "total_sources_known": true,
                "current_source": "/tmp/history\ncontrol.sqlite",
                "completed_records": 4096,
                "completed_bytes": 2048,
                "current_source_progress": {
                    "stage": "logical_scan",
                    "logical_rows_scanned": 4096,
                    "logical_certified_bytes": 2048
                }
            }
        }))
        .unwrap()
    }

    fn terminal_status() -> RefreshStatus {
        RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "published",
            "logical_request_id": "logical-request",
            "logical_phase": "terminal",
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": "published",
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": "published",
            "structured_outcome": {
                "code": "completed",
                "class": "completed",
                "retryable": false,
                "affected_routes": [],
                "retryable_routes": [],
                "blocked_routes": [],
                "physical_attempt_id": "physical-attempt"
            },
            "progress": {
                "phase": "committed",
                "completed_sources": 2,
                "total_sources": 2,
                "total_sources_known": true
            }
        }))
        .unwrap()
    }

    fn ui_with_stderr(
        stderr: SharedWriter,
        stderr_context: crate::ui::RenderContext,
    ) -> (Ui, SharedWriter) {
        let stdout = SharedWriter::default();
        let stdout_capture = stdout.clone();
        let stdout_context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stdout,
        ));
        (
            Ui::with_writers(stdout, stdout_context, stderr, stderr_context),
            stdout_capture,
        )
    }

    #[test]
    fn progress_mode_matrix_uses_injected_stderr_and_keeps_stdout_clean() {
        let cases = [
            (ProgressArg::Auto, true, false, false, true),
            (ProgressArg::Auto, false, false, false, false),
            (ProgressArg::Auto, true, false, true, false),
            (ProgressArg::Auto, true, true, false, false),
            (ProgressArg::Plain, false, false, false, true),
            (ProgressArg::Plain, true, false, false, true),
            (ProgressArg::Json, false, false, false, true),
            (ProgressArg::Json, true, false, false, true),
            (ProgressArg::None, true, false, false, false),
        ];
        for (arg, stderr_tty, term_dumb, final_json, expected_output) in cases {
            let stderr = SharedWriter::default();
            let stderr_capture = stderr.clone();
            let test_context = if stderr_tty {
                crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, 80).term_dumb(term_dumb)
            } else {
                crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr)
            };
            let (mut ui, stdout_capture) =
                ui_with_stderr(stderr, crate::ui::RenderContext::for_test(test_context));
            {
                let mut reporter = ProgressReporter::new(&mut ui, arg, final_json, "import", 0);
                reporter.source_refresh(&active_status()).unwrap();
            }
            assert_eq!(
                !stderr_capture.text().is_empty(),
                expected_output,
                "mode={arg:?}, tty={stderr_tty}, term_dumb={term_dumb}, final_json={final_json}"
            );
            assert!(stdout_capture.text().is_empty());
            if arg == ProgressArg::Plain {
                assert!(!stderr_capture.text().contains('\u{1b}'));
            }
            if arg == ProgressArg::Json {
                let value: serde_json::Value =
                    serde_json::from_str(stderr_capture.text().trim()).unwrap();
                assert_eq!(value["type"], "ctx_progress");
                assert_eq!(value["logical_phase"], "direct");
            }
        }
    }

    #[test]
    fn json_progress_releases_exactly_one_logical_terminal_done_event() {
        let stderr = SharedWriter::default();
        let capture = stderr.clone();
        let (mut ui, stdout) = ui_with_stderr(
            stderr,
            crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
                crate::ui::StreamKind::Stderr,
            )),
        );
        {
            let mut reporter = ProgressReporter::new(&mut ui, ProgressArg::Json, false, "setup", 0);
            reporter.source_refresh(&active_status()).unwrap();
            reporter.source_refresh(&terminal_status()).unwrap();
        }
        let events = capture
            .text()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events.iter().filter(|event| event["done"] == true).count(),
            1
        );
        let terminal = events.last().unwrap();
        assert_eq!(terminal["request_state"], "published");
        assert_eq!(terminal["structured_outcome"]["code"], "completed");
        assert!(stdout.text().is_empty());
    }

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
            refresh: None,
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
            refresh: None,
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
            refresh: None,
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
            refresh: None,
        };
        for (failure, expected) in [
            (WriterFailure::Write, "injected progress write failure"),
            (WriterFailure::Flush, "injected progress flush failure"),
        ] {
            let writer = FailingWriter(failure);
            let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
                crate::ui::StreamKind::Stderr,
            ));
            let mut output = LiveOutput::new(writer, context);
            let result = write_progress(
                &mut output,
                ProgressRenderMode::Json,
                "import",
                &line,
                StdDuration::ZERO,
            );
            assert!(result
                .expect_err("progress output failure must propagate")
                .to_string()
                .contains(expected));
        }
    }

    #[test]
    fn sqlite_logical_progress_is_typed_and_never_invents_a_total() {
        let snapshot = RefreshProgressSnapshot::from_status(&active_status()).unwrap();
        let line = source_refresh_line(snapshot, 0);
        assert_eq!(line.phase, "logical_scan");
        assert!(line.message.contains("history control.sqlite"));
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
        assert_eq!(value["logical_phase"], "direct");
        assert_eq!(value["physical_attempt_id"], "physical-attempt");
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

use std::{
    fmt,
    io::{self, Write},
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration as StdDuration, Instant},
};

use serde_json::json;

use crate::ui::{
    refresh_progress, CalloutPresentation, CalloutRow, CalloutStatus, Document, Line, LiveOutput,
    RefreshProgressSnapshot, Span, Token, Ui,
};

const MAX_PROGRESS_MESSAGE_BYTES: usize = 512;
const MAX_PROGRESS_SOURCE_BYTES: usize = 256;
const MAX_PROGRESS_PHASE_BYTES: usize = 64;
const LIVE_RENDER_INTERVAL: StdDuration = StdDuration::from_millis(100);
const LIVE_BACKEND_SILENCE_TIMEOUT: StdDuration = StdDuration::from_secs(5);

#[derive(Debug, Default)]
struct ActiveElapsedClock {
    displayed_millis: u64,
    observed_at: Option<StdDuration>,
    backend_snapshot_observed_at: Option<StdDuration>,
    backend_elapsed_millis_high_water: Option<u64>,
}

impl ActiveElapsedClock {
    fn advance(
        &mut self,
        reported_millis: Option<u64>,
        now: StdDuration,
        backend_snapshot_received: bool,
    ) -> u64 {
        let local_advance = self
            .observed_at
            .map(|observed_at| duration_millis(now.saturating_sub(observed_at)))
            .unwrap_or_default();
        self.displayed_millis = self
            .displayed_millis
            .saturating_add(local_advance)
            .max(reported_millis.unwrap_or_else(|| duration_millis(now)));
        self.observed_at = Some(now);
        if backend_snapshot_received {
            let first_snapshot = self.backend_snapshot_observed_at.is_none();
            let backend_clock_advanced = reported_millis
                .zip(self.backend_elapsed_millis_high_water)
                .is_some_and(|(reported, high_water)| reported > high_water);
            if first_snapshot || backend_clock_advanced {
                self.backend_snapshot_observed_at = Some(now);
            }
            if let Some(reported_millis) = reported_millis {
                self.backend_elapsed_millis_high_water = Some(
                    self.backend_elapsed_millis_high_water
                        .map_or(reported_millis, |high_water| {
                            high_water.max(reported_millis)
                        }),
                );
            }
        }
        self.displayed_millis
    }

    fn backend_snapshot_silent(&self, now: StdDuration) -> bool {
        self.backend_snapshot_observed_at
            .is_some_and(|observed_at| {
                now.saturating_sub(observed_at) >= LIVE_BACKEND_SILENCE_TIMEOUT
            })
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

fn duration_millis(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
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
pub struct ProgressWriterError(io::Error);

impl From<io::Error> for ProgressWriterError {
    fn from(error: io::Error) -> Self {
        Self(error)
    }
}

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

pub struct ProgressReporter<'a> {
    mode: ProgressRenderMode,
    operation: &'static str,
    total_bytes: u64,
    started: Instant,
    presentation_agent_histories: Option<Vec<String>>,
    persistent_callout: Option<CalloutPresentation>,
    output: ProgressOutput<'a>,
}

enum ProgressOutput<'a> {
    Direct(LiveOutput<&'a mut (dyn Write + Send)>),
    Live(LocalLiveRenderer),
}

impl<'a> ProgressOutput<'a> {
    fn direct_mut<'output>(
        &'output mut self,
    ) -> io::Result<&'output mut LiveOutput<&'a mut (dyn Write + Send)>> {
        match self {
            Self::Direct(output) => Ok(output),
            Self::Live(_) => Err(io::Error::other("live renderer has no direct writer")),
        }
    }

    fn write_live_document(&mut self, document: Document, final_frame: bool) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_document(document, final_frame),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }

    fn write_live_refresh(&mut self, snapshot: RefreshProgressSnapshot) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_refresh(snapshot),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }

    fn write_live_callout(&mut self, presentation: CalloutPresentation) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_callout(presentation),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }

    fn write_live_notice(&mut self, document: Document) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_notice(document),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }
}

enum LiveRenderCommand {
    Document {
        document: Document,
        final_frame: bool,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Refresh {
        snapshot: Box<RefreshProgressSnapshot>,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Callout {
        presentation: CalloutPresentation,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Notice {
        document: Document,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Shutdown,
}

struct LocalLiveRenderer {
    commands: mpsc::Sender<LiveRenderCommand>,
    background_error: Arc<Mutex<Option<io::Error>>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveRefreshPresentation {
    Shared,
    Setup,
}

impl LocalLiveRenderer {
    fn new(
        output: LiveOutput<Box<dyn Write + Send>>,
        started: Instant,
        presentation: LiveRefreshPresentation,
    ) -> Self {
        let (commands, receiver) = mpsc::channel();
        let background_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&background_error);
        let worker = thread::spawn(move || {
            run_live_renderer(output, receiver, started, presentation, &worker_error);
        });
        Self {
            commands,
            background_error,
            worker: Some(worker),
        }
    }

    fn write_document(&mut self, document: Document, final_frame: bool) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Document {
                document,
                final_frame,
                complete,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn write_refresh(&mut self, snapshot: RefreshProgressSnapshot) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Refresh {
                snapshot: Box::new(snapshot),
                complete,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn write_callout(&mut self, presentation: CalloutPresentation) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Callout {
                presentation,
                complete,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn write_notice(&mut self, document: Document) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Notice { document, complete })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn check_background_error(&self) -> io::Result<()> {
        let mut error = self
            .background_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        error.take().map_or(Ok(()), Err)
    }
}

impl Drop for LocalLiveRenderer {
    fn drop(&mut self) {
        let _ = self.commands.send(LiveRenderCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_live_renderer(
    mut output: LiveOutput<Box<dyn Write + Send>>,
    commands: mpsc::Receiver<LiveRenderCommand>,
    started: Instant,
    presentation: LiveRefreshPresentation,
    background_error: &Mutex<Option<io::Error>>,
) {
    let mut active = None;
    let mut persistent_callout = None;
    let mut persistent_notice = None;
    let mut clock = ActiveElapsedClock::default();
    loop {
        match commands.recv_timeout(LIVE_RENDER_INTERVAL) {
            Ok(LiveRenderCommand::Document {
                document,
                final_frame,
                complete,
            }) => {
                active = None;
                clock.reset();
                let result = output.write_frame(&document, final_frame);
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
            }
            Ok(LiveRenderCommand::Refresh { snapshot, complete }) => {
                let terminal = snapshot.is_terminal();
                let rendered =
                    prepare_live_snapshot((*snapshot).clone(), &mut clock, started.elapsed(), true);
                let result = output.render_frame(terminal, |context| {
                    render_live_refresh(
                        presentation,
                        context,
                        rendered,
                        persistent_notice.as_ref(),
                        persistent_callout.as_ref(),
                    )
                });
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
                active = (!terminal).then_some(*snapshot);
                if terminal {
                    clock.reset();
                }
            }
            Ok(LiveRenderCommand::Callout {
                presentation: callout,
                complete,
            }) => {
                persistent_callout = Some(callout);
                let result = if let Some(snapshot) = active.as_ref() {
                    output.render_frame(false, |context| {
                        let rendered = prepare_live_snapshot(
                            snapshot.clone(),
                            &mut clock,
                            started.elapsed(),
                            false,
                        );
                        render_live_refresh(
                            presentation,
                            context,
                            rendered,
                            persistent_notice.as_ref(),
                            persistent_callout.as_ref(),
                        )
                    })
                } else {
                    Ok(())
                };
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
            }
            Ok(LiveRenderCommand::Notice { document, complete }) => {
                persistent_notice = Some(document);
                let result = if let Some(snapshot) = active.as_ref() {
                    output.render_frame(false, |context| {
                        let rendered = prepare_live_snapshot(
                            snapshot.clone(),
                            &mut clock,
                            started.elapsed(),
                            false,
                        );
                        render_live_refresh(
                            presentation,
                            context,
                            rendered,
                            persistent_notice.as_ref(),
                            persistent_callout.as_ref(),
                        )
                    })
                } else {
                    Ok(())
                };
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
            }
            Ok(LiveRenderCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(snapshot) = active.as_ref() else {
                    continue;
                };
                let rendered =
                    prepare_live_snapshot(snapshot.clone(), &mut clock, started.elapsed(), false);
                if let Err(error) = output.render_frame(false, |context| {
                    render_live_refresh(
                        presentation,
                        context,
                        rendered,
                        persistent_notice.as_ref(),
                        persistent_callout.as_ref(),
                    )
                }) {
                    *background_error
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
                    break;
                }
            }
        }
    }
}

fn render_live_refresh(
    presentation: LiveRefreshPresentation,
    context: &crate::ui::RenderContext,
    mut snapshot: RefreshProgressSnapshot,
    persistent_notice: Option<&Document>,
    persistent_callout: Option<&CalloutPresentation>,
) -> Document {
    if presentation == LiveRefreshPresentation::Setup {
        snapshot.use_setup_live_presentation();
    }
    let mut document = refresh_progress(context, &snapshot);
    if let Some(notice) = persistent_notice {
        document.append(notice.clone());
    } else if let Some(callout) = persistent_callout {
        document.push_blank();
        document.append(callout.render(context));
    }
    document
}

fn prepare_live_snapshot(
    mut snapshot: RefreshProgressSnapshot,
    clock: &mut ActiveElapsedClock,
    now: StdDuration,
    backend_snapshot_received: bool,
) -> RefreshProgressSnapshot {
    if !snapshot.is_terminal() {
        let elapsed = clock.advance(
            snapshot.progress().elapsed_millis,
            now,
            backend_snapshot_received,
        );
        if clock.backend_snapshot_silent(now) {
            snapshot.suppress_stale_presentation_eta();
        }
        snapshot.advance_presentation_clock(elapsed);
    }
    snapshot
}

impl<'a> ProgressReporter<'a> {
    pub fn new(
        ui: &'a mut Ui,
        arg: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        Self::new_with_live_json_stderr(ui, arg, json_output, operation, total_bytes, false)
    }

    pub fn new_with_live_json_stderr(
        ui: &'a mut Ui,
        arg: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
        allow_live_json_stderr: bool,
    ) -> Self {
        let live_output_capable = ui.stderr_context().live_output_capable();
        let mode = match arg {
            ProgressMode::None => ProgressRenderMode::None,
            ProgressMode::Json => ProgressRenderMode::Json,
            ProgressMode::Plain => ProgressRenderMode::Plain,
            ProgressMode::Auto
                if !live_output_capable || (json_output && !allow_live_json_stderr) =>
            {
                ProgressRenderMode::None
            }
            ProgressMode::Auto => ProgressRenderMode::Live,
        };
        let started = Instant::now();
        let output = if mode == ProgressRenderMode::Live {
            let presentation = if operation == "setup" {
                LiveRefreshPresentation::Setup
            } else {
                LiveRefreshPresentation::Shared
            };
            ProgressOutput::Live(LocalLiveRenderer::new(
                ui.stderr_shared_live_output(),
                started,
                presentation,
            ))
        } else {
            ProgressOutput::Direct(ui.stderr_live_output())
        };
        Self {
            mode,
            operation,
            total_bytes,
            started,
            presentation_agent_histories: None,
            persistent_callout: None,
            output,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.mode != ProgressRenderMode::None
    }

    pub fn message(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.presentation_agent_histories = None;
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
            callout: None,
        })
    }

    /// Compatibility transport for the legacy Core setup progress modes.
    /// New structured-event consumers should compose a typed callout instead.
    pub fn notice(
        &mut self,
        phase: &'static str,
        lines: &[&str],
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() || lines.is_empty() {
            return Ok(());
        }
        self.presentation_agent_histories = None;
        let lines = lines
            .iter()
            .map(|line| bounded_progress_text(line, MAX_PROGRESS_MESSAGE_BYTES))
            .collect::<Vec<_>>();
        let message = lines.join("\n");
        let elapsed = self.started.elapsed();
        match self.mode {
            ProgressRenderMode::None => Ok(()),
            ProgressRenderMode::Live => {
                let mut document = Document::new();
                document.push_blank();
                for line in lines {
                    document.push_line(Line::new().with(Span::new(line, Token::Text)));
                }
                self.output
                    .write_live_notice(document)
                    .map_err(ProgressWriterError)
            }
            ProgressRenderMode::Plain => self
                .output
                .direct_mut()
                .and_then(|output| output.write_line(&message))
                .map_err(ProgressWriterError),
            ProgressRenderMode::Json => write_progress(
                &mut self.output,
                self.mode,
                self.operation,
                &ProgressLine {
                    phase: bounded_progress_text(phase, MAX_PROGRESS_PHASE_BYTES),
                    message,
                    completed_bytes: 0,
                    total_bytes: self.total_bytes,
                    completed_files: None,
                    total_files: None,
                    imported_events: None,
                    done: false,
                    refresh: None,
                    callout: None,
                },
                elapsed,
            )
            .map_err(ProgressWriterError),
        }
    }

    /// Installs or replaces one product-neutral callout beneath setup progress.
    /// Live output re-renders the owned facts at the destination's current
    /// width; plain output emits the callout only after terminal progress.
    pub fn callout(
        &mut self,
        phase: &'static str,
        presentation: CalloutPresentation,
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.presentation_agent_histories = None;
        self.persistent_callout = Some(presentation.clone());
        let message = bounded_progress_text(
            &presentation.plain_message(&crate::ui::RenderContext::canonical_human_measurement()),
            MAX_PROGRESS_MESSAGE_BYTES,
        );
        let elapsed = self.started.elapsed();
        match self.mode {
            ProgressRenderMode::None => Ok(()),
            ProgressRenderMode::Live => self
                .output
                .write_live_callout(presentation)
                .map_err(ProgressWriterError),
            ProgressRenderMode::Plain => Ok(()),
            ProgressRenderMode::Json => write_progress(
                &mut self.output,
                self.mode,
                self.operation,
                &ProgressLine {
                    phase: bounded_progress_text(phase, MAX_PROGRESS_PHASE_BYTES),
                    message,
                    completed_bytes: 0,
                    total_bytes: self.total_bytes,
                    completed_files: None,
                    total_files: None,
                    imported_events: None,
                    done: false,
                    refresh: None,
                    callout: Some(callout_json(&presentation)),
                },
                elapsed,
            )
            .map_err(ProgressWriterError),
        }
    }

    pub fn source_refresh(
        &mut self,
        snapshot: RefreshProgressSnapshot,
    ) -> Result<(), ProgressWriterError> {
        let now = self.started.elapsed();
        self.source_refresh_at(snapshot, now)
    }

    fn source_refresh_at(
        &mut self,
        mut snapshot: RefreshProgressSnapshot,
        now: StdDuration,
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() {
            return Ok(());
        }
        if self.mode == ProgressRenderMode::Live {
            if self.presentation_agent_histories.is_none()
                && snapshot.discovery_complete()
                && !snapshot.progress().agent_histories.is_empty()
            {
                self.presentation_agent_histories =
                    Some(snapshot.progress().agent_histories.clone());
            }
            snapshot.set_presentation_agent_histories(self.presentation_agent_histories.clone());
        }
        let line = source_refresh_line(snapshot, self.total_bytes);
        let terminal = line.done;
        write_progress(&mut self.output, self.mode, self.operation, &line, now)
            .map_err(ProgressWriterError)?;
        if terminal && self.mode == ProgressRenderMode::Plain {
            if let Some(callout) = self.persistent_callout.take() {
                let output = self.output.direct_mut().map_err(ProgressWriterError)?;
                let document = callout.render(output.context());
                output
                    .write_line(document.render_plain().trim_end_matches('\n'))
                    .map_err(ProgressWriterError)?;
            }
        }
        Ok(())
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
    callout: Option<serde_json::Value>,
}

fn write_progress(
    output: &mut ProgressOutput<'_>,
    mode: ProgressRenderMode,
    operation: &'static str,
    line: &ProgressLine,
    elapsed: StdDuration,
) -> io::Result<()> {
    match mode {
        ProgressRenderMode::None => Ok(()),
        ProgressRenderMode::Live => {
            if let Some(snapshot) = line.refresh.as_ref() {
                output.write_live_refresh(snapshot.clone())
            } else {
                let document =
                    Document::from_line(Line::new().with(Span::new(&line.message, Token::Text)));
                output.write_live_document(document, line.done)
            }
        }
        ProgressRenderMode::Plain => {
            let output = output.direct_mut()?;
            if let Some(snapshot) = line.refresh.as_ref() {
                let document = refresh_progress(output.context(), snapshot);
                output.write_line(document.render_plain().trim_end_matches('\n'))
            } else {
                output.write_line(&line.message)
            }
        }
        ProgressRenderMode::Json => output
            .direct_mut()?
            .write_line(&progress_json(operation, line, elapsed)),
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
        // Compatibility: this documented legacy field remains byte-rate based.
        // Source-backed consumers use estimated_remaining_millis below for the
        // explicit whole-run time until the refreshed generation is usable.
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
        value["agent_histories"] = json!(progress.agent_histories);
        value["processed_sessions"] = json!(progress.processed_sessions);
        value["processed_messages"] = json!(progress.processed_messages);
        value["processed_tool_calls"] = json!(progress.processed_tool_calls);
        value["processed_bytes"] = json!(progress.processed_bytes);
        value["whole_run_stage"] = json!(progress.whole_run_stage.as_str());
        value["estimated_remaining_millis"] = json!(progress.estimated_remaining_millis);
        value["refresh_elapsed_millis"] = json!(progress.elapsed_millis);
        value["current_source"] = json!(progress
            .current_source
            .as_deref()
            .map(|source| bounded_progress_text(source, MAX_PROGRESS_SOURCE_BYTES)));
        value["current_source_progress"] = progress
            .current_source_progress
            .as_ref()
            .map(crate::ui::RefreshCurrentSourceProgress::to_json)
            .unwrap_or(serde_json::Value::Null);
        snapshot.append_json_fields(&mut value);
    }
    if let Some(callout) = line.callout.as_ref() {
        value["callout"] = callout.clone();
    }
    value.to_string()
}

fn source_refresh_line(
    snapshot: RefreshProgressSnapshot,
    legacy_terminal_total_bytes: u64,
) -> ProgressLine {
    let (completed_bytes, engine_total_bytes) = snapshot.byte_progress();
    let phase = snapshot.phase();
    let message = snapshot.message();
    let done = snapshot.is_terminal();
    let total_bytes = if done && (completed_bytes, engine_total_bytes) == (0, 0) {
        legacy_terminal_total_bytes
    } else {
        engine_total_bytes
    };
    let imported_events = snapshot
        .progress()
        .completed_records
        .and_then(|value| usize::try_from(value).ok());
    ProgressLine {
        phase: bounded_progress_text(&phase, MAX_PROGRESS_PHASE_BYTES),
        message: bounded_progress_text(&message, MAX_PROGRESS_MESSAGE_BYTES),
        completed_bytes,
        total_bytes,
        completed_files: None,
        total_files: None,
        imported_events,
        done,
        refresh: Some(snapshot),
        callout: None,
    }
}

fn callout_json(presentation: &CalloutPresentation) -> serde_json::Value {
    let rows = presentation
        .rows()
        .iter()
        .map(|row| match row {
            CalloutRow::Blank => json!({"kind": "blank"}),
            CalloutRow::Text(text) => json!({"kind": "text", "text": text}),
            CalloutRow::Bullet(text) => json!({"kind": "bullet", "text": text}),
            CalloutRow::Status { level, text } => json!({
                "kind": "status",
                "level": match level {
                    CalloutStatus::Neutral => "neutral",
                    CalloutStatus::Success => "success",
                    CalloutStatus::Warning => "warning",
                    CalloutStatus::Failure => "failure",
                },
                "text": text,
            }),
            CalloutRow::Action(text) => json!({"kind": "action", "text": text}),
            CalloutRow::Reference(text) => json!({"kind": "reference", "text": text}),
            CalloutRow::Command(text) => json!({"kind": "command", "text": text}),
        })
        .collect::<Vec<_>>();
    json!({
        "title": presentation.title(),
        "rows": rows,
    })
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

pub fn format_bytes(bytes: u64) -> String {
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

pub fn format_count(value: u64) -> String {
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
mod tests;

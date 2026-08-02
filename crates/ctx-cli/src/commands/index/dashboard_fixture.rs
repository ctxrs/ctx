use std::process::ExitCode;

use anyhow::{bail, Context as _, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

use crate::{
    cli::IndexDashboardFixtureArgs,
    ui::{RenderContext, Ui},
};

pub(crate) const COMMAND_NAME: &str = "_index-dashboard-renderer-fixture";
pub(crate) const FIXTURE_CLOCK: &str = "2026-06-23T12:00:00Z";
pub(crate) const FIXTURE_RANDOM_SEED: &str = "ctx-cli-ux-core-v1";

const FIXTURE_ROWS: usize = 24;
const FIXTURE_COLUMNS: &[usize] = &[32, 80];
const INDEXED_ITEMS: u64 = 854_466;
const INDEXED_SESSIONS: u64 = 3_486;
const CERTIFIED_SOURCE_BYTES: u64 = 10_700_000_000;
const COMPLETED_SOURCES: u64 = 7;
const TOTAL_SOURCES: u64 = 12;
const EMBEDDED_ITEMS: u64 = 357_421;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum FixtureCase {
    Discovering,
    ActiveProgress,
    Finalizing,
    Ready,
    ReadyPartialWarning,
    TerminalFailure,
    StoppedDaemon,
    SemanticDisabled,
    SemanticProgress,
    SemanticReady,
    SemanticFailure,
}

pub(crate) fn parse_columns(value: &str) -> std::result::Result<usize, String> {
    let columns = value
        .parse::<usize>()
        .map_err(|_| format!("unsupported fixture column count {value:?}; expected 32 or 80"))?;
    if FIXTURE_COLUMNS.contains(&columns) {
        Ok(columns)
    } else {
        Err(format!(
            "unsupported fixture column count {columns}; expected 32 or 80"
        ))
    }
}

pub(crate) fn parse_rows(value: &str) -> std::result::Result<usize, String> {
    let rows = value
        .parse::<usize>()
        .map_err(|_| format!("unsupported fixture row count {value:?}; expected 24"))?;
    if rows == FIXTURE_ROWS {
        Ok(rows)
    } else {
        Err(format!(
            "unsupported fixture row count {rows}; expected {FIXTURE_ROWS}"
        ))
    }
}

pub(crate) fn run(args: IndexDashboardFixtureArgs, ui: &mut Ui) -> Result<ExitCode> {
    validate_parameters(&args)?;
    let context = *ui.stdout_context();
    validate_terminal(&context, args.columns, args.rows, detected_stdout_size())?;

    let mut output = super::index_watch_output(ui);
    for status in args.case.status_sequence()? {
        // Each fixture frame is an independent production snapshot while
        // preserving IndexWatchOutput's real redraw state.
        output.dashboard = Default::default();
        output.print_human(&status)?;
    }
    Ok(args.case.exit_code())
}

fn validate_parameters(args: &IndexDashboardFixtureArgs) -> Result<()> {
    if args.clock != FIXTURE_CLOCK {
        bail!(
            "unsupported fixture clock {:?}; expected {FIXTURE_CLOCK}",
            args.clock
        );
    }
    if args.random_seed != FIXTURE_RANDOM_SEED {
        bail!(
            "unsupported fixture random seed {:?}; expected {FIXTURE_RANDOM_SEED}",
            args.random_seed
        );
    }
    Ok(())
}

fn detected_stdout_size() -> Option<(usize, usize)> {
    terminal_size::terminal_size().map(
        |(terminal_size::Width(columns), terminal_size::Height(rows))| {
            (usize::from(columns), usize::from(rows))
        },
    )
}

fn validate_terminal(
    context: &RenderContext,
    expected_columns: usize,
    expected_rows: usize,
    detected_size: Option<(usize, usize)>,
) -> Result<()> {
    if !context.is_terminal() {
        bail!("index dashboard fixture requires stdout to be a terminal");
    }
    let (columns, rows) =
        detected_size.context("index dashboard fixture could not detect stdout terminal size")?;
    if context.terminal_width() != Some(columns) {
        bail!("index dashboard fixture detected inconsistent stdout terminal widths");
    }
    if (columns, rows) != (expected_columns, expected_rows) {
        bail!(
            "index dashboard fixture expected a {expected_columns}x{expected_rows} terminal, \
             detected {columns}x{rows}"
        );
    }
    Ok(())
}

impl FixtureCase {
    fn exit_code(self) -> ExitCode {
        if matches!(
            self,
            Self::TerminalFailure | Self::StoppedDaemon | Self::SemanticFailure
        ) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }

    fn status_sequence(self) -> Result<Vec<Value>> {
        let mut statuses = Vec::with_capacity(2);
        if let Some(previous) = self.previous_case() {
            statuses.push(previous.status()?);
        }
        statuses.push(self.status()?);
        Ok(statuses)
    }

    fn previous_case(self) -> Option<Self> {
        match self {
            Self::Discovering => None,
            Self::ActiveProgress => Some(Self::Discovering),
            Self::Finalizing | Self::TerminalFailure | Self::StoppedDaemon => {
                Some(Self::ActiveProgress)
            }
            Self::Ready | Self::ReadyPartialWarning | Self::SemanticDisabled => {
                Some(Self::Finalizing)
            }
            Self::SemanticProgress => Some(Self::SemanticDisabled),
            Self::SemanticReady | Self::SemanticFailure => Some(Self::SemanticProgress),
        }
    }

    fn status(self) -> Result<Value> {
        serde_json::to_value(self.typed_status())
            .context("serialize index dashboard fixture status")
    }

    fn typed_status(self) -> DashboardStatus {
        let mut status = DashboardStatus::active();
        match self {
            Self::Discovering => {
                status.initialized = false;
                status.lexical.status = LexicalState::Pending;
                status.lexical.indexed_items = 0;
                status.lexical.indexed_sessions = 0;
                status.lexical.indexed_sources = 0;
                status.lexical.certified_source_bytes = 0;
                status.refresh.progress.completed_sources = 0;
                status.refresh.progress.total_sources = 0;
            }
            Self::ActiveProgress => {}
            Self::Finalizing => {
                status.refresh.progress.phase = "publishing";
                status.refresh.progress.completed_sources = TOTAL_SOURCES;
            }
            Self::Ready | Self::SemanticDisabled => {
                status.make_lexical_ready();
            }
            Self::ReadyPartialWarning => {
                status.make_lexical_ready();
                status.refresh.status = RefreshState::Pending;
                status.refresh.reason = Some("core_refresh_pending");
                status.daemon.running = false;
            }
            Self::TerminalFailure => {
                status.initialized = false;
                status.lexical.status = LexicalState::Unavailable;
                status.lexical.reason = Some("generation_verification_failed");
                status.refresh.status = RefreshState::Unavailable;
                status.refresh.reason = Some("core_refresh_failed");
            }
            Self::StoppedDaemon => {
                status.daemon.status = DaemonState::Failed;
                status.daemon.running = false;
                status.daemon.enabled = false;
            }
            Self::SemanticProgress => {
                status.make_lexical_ready();
                status.enable_semantic(SemanticState::Pending, None, EMBEDDED_ITEMS);
            }
            Self::SemanticReady => {
                status.make_lexical_ready();
                status.enable_semantic(SemanticState::Ready, None, INDEXED_ITEMS);
            }
            Self::SemanticFailure => {
                status.make_lexical_ready();
                status.enable_semantic(
                    SemanticState::Failed,
                    Some("embedding_runtime_failed"),
                    EMBEDDED_ITEMS,
                );
                status.daemon.jobs.semantic_index.status = SemanticState::Failed;
                status.daemon.jobs.semantic_index.reason = Some("embedding_runtime_failed");
            }
        }
        status
    }
}

#[derive(Debug, Serialize)]
struct DashboardStatus {
    initialized: bool,
    lexical: LexicalStatus,
    refresh: RefreshStatus,
    semantic: SemanticStatus,
    daemon: DaemonStatus,
}

impl DashboardStatus {
    fn active() -> Self {
        Self {
            initialized: true,
            lexical: LexicalStatus {
                status: LexicalState::Ready,
                reason: None,
                indexed_items: INDEXED_ITEMS,
                indexed_sessions: INDEXED_SESSIONS,
                indexed_sources: TOTAL_SOURCES,
                certified_source_bytes: CERTIFIED_SOURCE_BYTES,
            },
            refresh: RefreshStatus {
                status: RefreshState::Pending,
                reason: Some("core_refresh_pending"),
                progress: RefreshProgress {
                    phase: "scanning_provider_sources",
                    completed_sources: COMPLETED_SOURCES,
                    total_sources: TOTAL_SOURCES,
                },
            },
            semantic: SemanticStatus {
                status: SemanticState::Disabled,
                reason: Some("semantic_disabled"),
                enabled: false,
                coverage: SemanticCoverage {
                    embedded_items: 0,
                    searchable_items: INDEXED_ITEMS,
                },
            },
            daemon: DaemonStatus {
                status: DaemonState::Running,
                running: true,
                enabled: true,
                jobs: DaemonJobs {
                    semantic_index: SemanticJob {
                        status: SemanticState::Disabled,
                        reason: Some("semantic_disabled"),
                    },
                },
            },
        }
    }

    fn make_lexical_ready(&mut self) {
        self.lexical.status = LexicalState::Ready;
        self.lexical.reason = None;
        self.refresh.status = RefreshState::Ready;
        self.refresh.reason = None;
        self.refresh.progress.phase = "published";
        self.refresh.progress.completed_sources = TOTAL_SOURCES;
    }

    fn enable_semantic(
        &mut self,
        state: SemanticState,
        reason: Option<&'static str>,
        embedded_items: u64,
    ) {
        self.semantic.status = state;
        self.semantic.reason = reason;
        self.semantic.enabled = true;
        self.semantic.coverage.embedded_items = embedded_items;
        self.daemon.jobs.semantic_index.status = state;
        self.daemon.jobs.semantic_index.reason = reason;
    }
}

#[derive(Debug, Serialize)]
struct LexicalStatus {
    status: LexicalState,
    reason: Option<&'static str>,
    indexed_items: u64,
    indexed_sessions: u64,
    indexed_sources: u64,
    certified_source_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum LexicalState {
    Pending,
    Ready,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct RefreshStatus {
    status: RefreshState,
    reason: Option<&'static str>,
    progress: RefreshProgress,
}

#[derive(Debug, Serialize)]
struct RefreshProgress {
    phase: &'static str,
    completed_sources: u64,
    total_sources: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RefreshState {
    Pending,
    Ready,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct SemanticStatus {
    status: SemanticState,
    reason: Option<&'static str>,
    enabled: bool,
    coverage: SemanticCoverage,
}

#[derive(Debug, Serialize)]
struct SemanticCoverage {
    embedded_items: u64,
    searchable_items: u64,
}

#[derive(Debug, Serialize)]
struct DaemonStatus {
    status: DaemonState,
    running: bool,
    enabled: bool,
    jobs: DaemonJobs,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum DaemonState {
    Running,
    Failed,
}

#[derive(Debug, Serialize)]
struct DaemonJobs {
    semantic_index: SemanticJob,
}

#[derive(Debug, Serialize)]
struct SemanticJob {
    status: SemanticState,
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SemanticState {
    Disabled,
    Pending,
    Ready,
    Failed,
}

#[cfg(test)]
#[path = "dashboard_fixture/tests.rs"]
mod tests;

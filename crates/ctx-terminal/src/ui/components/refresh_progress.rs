use serde_json::{json, Value};

use crate::{format_bytes, format_count};

use super::{fields, fields::fields_with_label_width, progress, Field, Progress};
use crate::ui::{Document, RenderContext};

const MAX_DYNAMIC_TEXT_BYTES: usize = 256;
const REFRESH_TABLE_LABEL_WIDTH: usize = "Estimated remaining".len();
const MAX_PROGRESS_BAR_WIDTH: usize = 48;
const PROGRESS_PULSE_WIDTH: usize = 8;
const MIN_USEFUL_REMAINING_MILLIS: u64 = 2_000;

/// Terminal-neutral presentation view of a refresh status. Composition code
/// converts its domain snapshot to this owned value before rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshProgressSnapshot {
    request_id: Option<String>,
    kind: RefreshStatusKind,
    progress: RefreshProgress,
    total_sources_known: bool,
    presentation_agent_histories: Option<Vec<String>>,
    presentation: RefreshProgressPresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshProgressPresentation {
    Shared,
    SetupLive,
}

impl RefreshProgressSnapshot {
    pub fn new(
        request_id: Option<String>,
        kind: RefreshStatusKind,
        progress: RefreshProgress,
        total_sources_known: bool,
    ) -> Self {
        let presentation_agent_histories = (total_sources_known
            && !matches!(
                progress.phase.as_str(),
                "queued" | "pending" | "discovering"
            )
            && !progress.agent_histories.is_empty())
        .then(|| progress.agent_histories.clone());
        Self {
            request_id,
            kind,
            progress,
            total_sources_known,
            presentation_agent_histories,
            presentation: RefreshProgressPresentation::Shared,
        }
    }

    pub const fn kind(&self) -> &RefreshStatusKind {
        &self.kind
    }

    pub const fn progress(&self) -> &RefreshProgress {
        &self.progress
    }

    #[cfg(test)]
    pub(crate) fn progress_mut_for_test(&mut self) -> &mut RefreshProgress {
        &mut self.progress
    }

    pub const fn total_sources_known(&self) -> bool {
        self.total_sources_known
    }

    pub const fn whole_run_stage(&self) -> RefreshWholeRunStage {
        self.progress.whole_run_stage
    }

    pub const fn estimated_remaining_millis(&self) -> Option<u64> {
        self.progress.estimated_remaining_millis
    }

    pub(crate) fn discovery_complete(&self) -> bool {
        self.total_sources_known
            && !matches!(
                self.progress.phase.as_str(),
                "queued" | "pending" | "discovering"
            )
    }

    /// Advances only the local human display clock. Backend counters and JSON
    /// progress retain the daemon snapshot supplied by the caller.
    pub(crate) fn advance_presentation_clock(&mut self, elapsed_millis: u64) {
        if !self.is_terminal() {
            let backend_elapsed = self.progress.elapsed_millis.unwrap_or(elapsed_millis);
            if let Some(remaining) = self.progress.estimated_remaining_millis {
                let local_advance = elapsed_millis.saturating_sub(backend_elapsed);
                self.progress.estimated_remaining_millis = remaining
                    .checked_sub(local_advance)
                    .filter(|remaining| *remaining > MIN_USEFUL_REMAINING_MILLIS);
            }
            self.progress.elapsed_millis = Some(elapsed_millis);
        }
    }

    /// Clears only presentation ETA derived from a stale backend snapshot.
    pub(crate) fn suppress_stale_presentation_eta(&mut self) {
        self.progress.estimated_remaining_millis = None;
    }

    pub(crate) fn set_presentation_agent_histories(&mut self, histories: Option<Vec<String>>) {
        self.presentation_agent_histories = histories;
    }

    pub(crate) fn use_setup_live_presentation(&mut self) {
        self.presentation = RefreshProgressPresentation::SetupLive;
    }

    /// Reconciles a successful setup's terminal presentation with the
    /// committed index. Live refresh counters describe in-flight work and may
    /// legitimately be empty when a fast or coalesced refresh publishes.
    pub fn set_terminal_history_totals(
        &mut self,
        sessions: u64,
        messages: u64,
        tool_calls: u64,
        source_bytes: u64,
    ) {
        debug_assert!(self.is_terminal());
        self.progress.processed_sessions = sessions;
        self.progress.processed_messages = messages;
        self.progress.processed_tool_calls = tool_calls;
        self.progress.processed_bytes = source_bytes;
    }

    pub fn is_terminal(&self) -> bool {
        self.kind.request_state().is_terminal()
    }

    pub fn phase(&self) -> String {
        if self.is_terminal() {
            return match self.kind.request_state() {
                RefreshRequestState::Published => "published".to_owned(),
                RefreshRequestState::Failed => "failed".to_owned(),
                RefreshRequestState::AdmissionPending
                | RefreshRequestState::Queued
                | RefreshRequestState::Running => unreachable!("terminal status is not active"),
            };
        }
        self.progress
            .current_source_progress
            .as_ref()
            .map(|current| current.stage.as_str().to_owned())
            .unwrap_or_else(|| self.progress.phase.clone())
    }

    pub fn message(&self) -> String {
        let label = machine_refresh_label(self);
        let sources = source_count_text(self);
        match self.progress.current_source.as_deref() {
            Some(source) if !self.is_terminal() => {
                format!("{label}: {} ({sources}).", bounded_dynamic_text(source))
            }
            _ => format!("{label} ({sources})."),
        }
    }

    pub fn byte_progress(&self) -> (u64, u64) {
        let Some(current) = self.progress.current_source_progress.as_ref() else {
            return (0, 0);
        };
        match current.stage {
            RefreshCurrentSourceProgressStage::SourceFamilyCopy
            | RefreshCurrentSourceProgressStage::OnlineBackup => current
                .snapshot_bytes_completed
                .zip(current.snapshot_bytes_total)
                .unwrap_or((0, 0)),
            RefreshCurrentSourceProgressStage::LogicalFingerprint
            | RefreshCurrentSourceProgressStage::LogicalScan
            | RefreshCurrentSourceProgressStage::Parsing
            | RefreshCurrentSourceProgressStage::IndexWriting => (0, 0),
        }
    }

    pub(crate) fn append_json_fields(&self, value: &mut Value) {
        if let Some(request_id) = self.request_id.as_ref() {
            value["request_id"] = json!(request_id);
        }
        value["request_state"] = json!(self.kind.request_state().as_str());
        match &self.kind {
            RefreshStatusKind::Legacy { .. } => {}
            RefreshStatusKind::BackgroundMaintenanceWake => {
                if let Some(request_id) = self.request_id.as_ref() {
                    value["logical_request_id"] = json!(request_id);
                }
                value["logical_phase"] = json!(RefreshLogicalPhase::Waiting.as_str());
                value["maintenance_wake"] = json!(true);
            }
            RefreshStatusKind::Logical(logical) => {
                if let Some(request_id) = self.request_id.as_ref() {
                    value["logical_request_id"] = json!(request_id);
                }
                value["logical_phase"] = json!(logical.logical_phase.as_str());
                value["physical_attempt_id"] = json!(logical.physical_attempt_id);
                value["physical_attempt_state"] = json!(logical.physical_attempt_state.as_str());
                value["progress_owner_request_id"] = json!(logical.progress_owner_request_id);
                value["progress_owner_attempt_state"] =
                    json!(logical.progress_owner_attempt_state.as_str());
                if let Some(outcome) = logical.structured_outcome.as_ref() {
                    value["structured_outcome"] = outcome.to_json();
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshRequestState {
    AdmissionPending,
    Queued,
    Running,
    Published,
    Failed,
}
impl RefreshRequestState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Failed)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionPending => "admission_pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshLogicalPhase {
    Waiting,
    Attached,
    CoverageCheck,
    ExactSuccessor,
    Direct,
    Terminal,
}
impl RefreshLogicalPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Attached => "attached",
            Self::CoverageCheck => "coverage_check",
            Self::ExactSuccessor => "exact_successor",
            Self::Direct => "direct",
            Self::Terminal => "terminal",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshStatusKind {
    Legacy { request_state: RefreshRequestState },
    BackgroundMaintenanceWake,
    Logical(RefreshLogicalStatus),
}
impl RefreshStatusKind {
    pub const fn request_state(&self) -> RefreshRequestState {
        match self {
            Self::Legacy { request_state } => *request_state,
            Self::BackgroundMaintenanceWake => RefreshRequestState::Queued,
            Self::Logical(logical) => logical.request_state,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshLogicalStatus {
    pub request_state: RefreshRequestState,
    pub logical_phase: RefreshLogicalPhase,
    pub physical_attempt_id: String,
    pub physical_attempt_state: RefreshRequestState,
    pub progress_owner_request_id: String,
    pub progress_owner_attempt_state: RefreshRequestState,
    pub structured_outcome: Option<Box<RefreshStructuredOutcome>>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStructuredOutcome {
    pub code: String,
    pub class: String,
    pub retryable: bool,
    pub affected_routes: Vec<String>,
    pub retryable_routes: Vec<String>,
    pub blocked_routes: Vec<String>,
    pub physical_attempt_id: String,
    pub retained_generation: Option<String>,
    pub published_generation: Option<String>,
    pub retry_advice: Option<String>,
    pub detail: Option<String>,
    pub failure: bool,
}
impl RefreshStructuredOutcome {
    fn is_failure(&self) -> bool {
        self.failure
    }

    fn to_json(&self) -> Value {
        let mut value = json!({
            "code": self.code,
            "class": self.class,
            "retryable": self.retryable,
            "affected_routes": self.affected_routes,
            "retryable_routes": self.retryable_routes,
            "blocked_routes": self.blocked_routes,
            "physical_attempt_id": self.physical_attempt_id,
            "retained_generation": self.retained_generation,
            "published_generation": self.published_generation,
            "retry_advice": self.retry_advice,
            "detail": self.detail,
        });
        if let Value::Object(fields) = &mut value {
            fields.retain(|_, field| !field.is_null());
        }
        value
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshProgress {
    pub phase: String,
    pub completed_sources: u64,
    pub total_sources: u64,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub agent_histories: Vec<String>,
    pub processed_sessions: u64,
    pub processed_messages: u64,
    pub processed_tool_calls: u64,
    pub processed_bytes: u64,
    pub elapsed_millis: Option<u64>,
    pub whole_run_stage: RefreshWholeRunStage,
    pub estimated_remaining_millis: Option<u64>,
    pub current_source_progress: Option<RefreshCurrentSourceProgress>,
}

impl Default for RefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            agent_histories: Vec::new(),
            processed_sessions: 0,
            processed_messages: 0,
            processed_tool_calls: 0,
            processed_bytes: 0,
            elapsed_millis: None,
            whole_run_stage: RefreshWholeRunStage::Preparing,
            estimated_remaining_millis: None,
            current_source_progress: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RefreshWholeRunStage {
    #[default]
    Preparing,
    Reading,
    Merging,
    Syncing,
    PhysicalVerification,
    LogicalVerification,
    Activation,
    Complete,
    Failed,
}

impl RefreshWholeRunStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Reading => "reading",
            Self::Merging => "merging",
            Self::Syncing => "syncing",
            Self::PhysicalVerification => "physical_verification",
            Self::LogicalVerification => "logical_verification",
            Self::Activation => "activation",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
    Parsing,
    IndexWriting,
}
impl RefreshCurrentSourceProgressStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
            Self::Parsing => "parsing",
            Self::IndexWriting => "index_writing",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshCurrentSourceProgress {
    pub stage: RefreshCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl RefreshCurrentSourceProgress {
    pub(crate) fn to_json(&self) -> Value {
        let mut value = json!({
            "stage": self.stage.as_str(),
            "snapshot_pages_completed": self.snapshot_pages_completed,
            "snapshot_pages_total": self.snapshot_pages_total,
            "snapshot_bytes_completed": self.snapshot_bytes_completed,
            "snapshot_bytes_total": self.snapshot_bytes_total,
            "logical_rows_scanned": self.logical_rows_scanned,
            "logical_certified_bytes": self.logical_certified_bytes,
        });
        if let Value::Object(fields) = &mut value {
            fields.retain(|_, field| !field.is_null());
        }
        value
    }
}

pub fn refresh_progress(context: &RenderContext, snapshot: &RefreshProgressSnapshot) -> Document {
    match snapshot.presentation {
        RefreshProgressPresentation::Shared => shared_refresh_progress(context, snapshot),
        RefreshProgressPresentation::SetupLive => setup_live_refresh_progress(context, snapshot),
    }
}

fn shared_refresh_progress(
    context: &RenderContext,
    snapshot: &RefreshProgressSnapshot,
) -> Document {
    let label = shared_refresh_label(snapshot);
    let mut document = progress(
        context,
        Progress {
            label,
            current: if snapshot.is_terminal() {
                1
            } else {
                snapshot.progress.elapsed_millis.unwrap_or(0) / 250
            },
            total: snapshot.is_terminal().then_some(1),
            detail: None,
        },
    );

    let sessions = format_count(snapshot.progress.processed_sessions);
    let messages = format_count(snapshot.progress.processed_messages);
    let tool_calls = format_count(snapshot.progress.processed_tool_calls);
    let scanned = format_bytes(snapshot.progress.processed_bytes);
    let elapsed = snapshot
        .progress
        .elapsed_millis
        .map(format_duration_millis)
        .unwrap_or_else(|| "measuring".to_owned());
    let remaining = if snapshot.is_terminal() {
        "complete".to_owned()
    } else {
        "estimating".to_owned()
    };
    let histories = if snapshot.progress.agent_histories.is_empty() {
        vec!["discovering".to_owned()]
    } else {
        snapshot.progress.agent_histories.clone()
    };
    let mut detail_fields = Vec::with_capacity(histories.len().saturating_add(6));
    for (index, history) in histories.iter().enumerate() {
        if index == 0 {
            detail_fields.push(Field::new("Agent histories", history));
        } else {
            detail_fields.push(Field::continuation(history));
        }
    }
    detail_fields.extend([
        Field::new("Sessions", &sessions),
        Field::new("Messages", &messages),
        Field::new("Tool calls", &tool_calls),
        Field::new("Data scanned", &scanned),
        Field::new("Elapsed", &elapsed),
        Field::new("Remaining", &remaining),
    ]);
    document.push_blank();
    document.append(fields(context, &detail_fields));
    document
}

fn shared_refresh_label(snapshot: &RefreshProgressSnapshot) -> &'static str {
    match &snapshot.kind {
        RefreshStatusKind::BackgroundMaintenanceWake => "History refresh is queued",
        RefreshStatusKind::Legacy { request_state } => match request_state {
            RefreshRequestState::AdmissionPending | RefreshRequestState::Queued => {
                "History refresh is queued"
            }
            RefreshRequestState::Running => transient_activity_label(snapshot)
                .unwrap_or_else(|| shared_physical_label(&snapshot.progress.phase)),
            RefreshRequestState::Published => "History refresh complete",
            RefreshRequestState::Failed => "History refresh failed",
        },
        RefreshStatusKind::Logical(logical) => match logical.logical_phase {
            RefreshLogicalPhase::Waiting => "History refresh is waiting",
            RefreshLogicalPhase::Attached => {
                transient_activity_label(snapshot).unwrap_or("Refreshing history with shared work")
            }
            RefreshLogicalPhase::CoverageCheck => "Checking refresh coverage",
            RefreshLogicalPhase::ExactSuccessor => "Waiting for successor refresh",
            RefreshLogicalPhase::Direct => transient_activity_label(snapshot)
                .unwrap_or_else(|| shared_physical_label(&snapshot.progress.phase)),
            RefreshLogicalPhase::Terminal => terminal_label(logical),
        },
    }
}

fn shared_physical_label(phase: &str) -> &'static str {
    match phase {
        "queued" | "pending" | "discovering" => "Discovering history sources",
        "committing" | "committed" | "publishing" => "Publishing search index",
        "verifying" => "Verifying refreshed history",
        _ => "Indexing your agent history",
    }
}

fn setup_live_refresh_progress(
    context: &RenderContext,
    snapshot: &RefreshProgressSnapshot,
) -> Document {
    let label = human_refresh_label(snapshot);
    let (bar_current, bar_total) = if snapshot.is_terminal() {
        (1, Some(1))
    } else if let (Some(elapsed), Some(remaining)) = (
        snapshot.progress.elapsed_millis,
        snapshot.estimated_remaining_millis(),
    ) {
        (elapsed, Some(elapsed.saturating_add(remaining).max(1)))
    } else {
        (
            indeterminate_position(context, snapshot.progress.elapsed_millis.unwrap_or(0)),
            None,
        )
    };
    let mut document = progress(
        context,
        Progress {
            label,
            current: bar_current,
            total: bar_total,
            detail: None,
        },
    );

    let sessions = format_count(snapshot.progress.processed_sessions);
    let messages = format_count(snapshot.progress.processed_messages);
    let tool_calls = format_count(snapshot.progress.processed_tool_calls);
    let scanned = format_bytes(snapshot.progress.processed_bytes);
    let elapsed = snapshot
        .progress
        .elapsed_millis
        .map(format_duration_millis)
        .unwrap_or_else(|| "measuring".to_owned());
    let remaining = snapshot
        .estimated_remaining_millis()
        .map(format_eta_duration_millis)
        .unwrap_or_else(|| "Estimating".to_owned());
    let Some(histories) = snapshot.presentation_agent_histories.as_ref() else {
        return document;
    };
    let mut history_fields = Vec::with_capacity(histories.len());
    for (index, history) in histories.iter().enumerate() {
        if index == 0 {
            history_fields.push(Field::new("Agent histories", history));
        } else {
            history_fields.push(Field::continuation(history));
        }
    }
    let mut metric_fields = vec![
        Field::new("Sessions", &sessions),
        Field::new("Messages", &messages),
        Field::new("Tool calls", &tool_calls),
        Field::new("Data scanned", &scanned),
        Field::new("Elapsed", &elapsed),
    ];
    if !snapshot.is_terminal() {
        metric_fields.push(Field::new("Estimated remaining", &remaining));
    }
    document.push_blank();
    document.append(fields_with_label_width(
        context,
        &history_fields,
        REFRESH_TABLE_LABEL_WIDTH,
    ));
    document.push_blank();
    document.append(fields_with_label_width(
        context,
        &metric_fields,
        REFRESH_TABLE_LABEL_WIDTH,
    ));
    document
}

fn indeterminate_position(context: &RenderContext, elapsed_millis: u64) -> u64 {
    let bar_width = context
        .content_width()
        .map_or(MAX_PROGRESS_BAR_WIDTH, |width| {
            width.min(MAX_PROGRESS_BAR_WIDTH)
        })
        .max(1);
    let travel = bar_width.saturating_sub(bar_width.min(PROGRESS_PULSE_WIDTH));
    if travel == 0 {
        return 0;
    }
    let travel = u64::try_from(travel).unwrap_or(u64::MAX);
    let tick = elapsed_millis / 100;
    let cycle = travel.saturating_mul(2);
    let phase = tick % cycle;
    if phase <= travel {
        phase
    } else {
        cycle - phase
    }
}

fn human_refresh_label(snapshot: &RefreshProgressSnapshot) -> &'static str {
    match &snapshot.kind {
        RefreshStatusKind::BackgroundMaintenanceWake => "Preparing your history",
        RefreshStatusKind::Legacy { request_state } => match request_state {
            RefreshRequestState::AdmissionPending | RefreshRequestState::Queued => {
                "Preparing your history"
            }
            RefreshRequestState::Running => transient_activity_label(snapshot)
                .unwrap_or_else(|| whole_run_label(snapshot.whole_run_stage())),
            RefreshRequestState::Published => "History refresh complete",
            RefreshRequestState::Failed => "History refresh failed",
        },
        RefreshStatusKind::Logical(logical) => match logical.logical_phase {
            RefreshLogicalPhase::Waiting
            | RefreshLogicalPhase::CoverageCheck
            | RefreshLogicalPhase::ExactSuccessor => "Preparing your history",
            RefreshLogicalPhase::Attached | RefreshLogicalPhase::Direct => {
                transient_activity_label(snapshot)
                    .unwrap_or_else(|| whole_run_label(snapshot.whole_run_stage()))
            }
            RefreshLogicalPhase::Terminal => terminal_label(logical),
        },
    }
}

fn transient_activity_label(snapshot: &RefreshProgressSnapshot) -> Option<&'static str> {
    match snapshot
        .progress
        .current_source_progress
        .as_ref()
        .map(|progress| progress.stage)
    {
        Some(RefreshCurrentSourceProgressStage::Parsing) => Some("Parsing your agent history"),
        Some(RefreshCurrentSourceProgressStage::IndexWriting) => Some("Writing search index"),
        Some(
            RefreshCurrentSourceProgressStage::SourceFamilyCopy
            | RefreshCurrentSourceProgressStage::OnlineBackup
            | RefreshCurrentSourceProgressStage::LogicalFingerprint
            | RefreshCurrentSourceProgressStage::LogicalScan,
        )
        | None => None,
    }
}

fn whole_run_label(stage: RefreshWholeRunStage) -> &'static str {
    match stage {
        RefreshWholeRunStage::Preparing => "Preparing your history",
        RefreshWholeRunStage::Reading => "Reading your agent history",
        RefreshWholeRunStage::Merging => "Merging search index",
        RefreshWholeRunStage::Syncing => "Syncing search index",
        RefreshWholeRunStage::PhysicalVerification => "Verifying search index files",
        RefreshWholeRunStage::LogicalVerification => "Verifying indexed history",
        RefreshWholeRunStage::Activation => "Activating search index",
        RefreshWholeRunStage::Complete => "History refresh complete",
        RefreshWholeRunStage::Failed => "History refresh failed",
    }
}

// Machine progress messages remain a separate compatibility surface from the
// live human frame and retain their established request and phase labels.
fn machine_refresh_label(snapshot: &RefreshProgressSnapshot) -> &'static str {
    match &snapshot.kind {
        RefreshStatusKind::BackgroundMaintenanceWake => "History refresh is queued",
        RefreshStatusKind::Legacy { request_state } => match request_state {
            RefreshRequestState::AdmissionPending | RefreshRequestState::Queued => {
                "History refresh is queued"
            }
            RefreshRequestState::Running => physical_label(&snapshot.progress.phase),
            RefreshRequestState::Published => "History refresh complete",
            RefreshRequestState::Failed => "History refresh failed",
        },
        RefreshStatusKind::Logical(logical) => match logical.logical_phase {
            RefreshLogicalPhase::Waiting => "History refresh is waiting",
            RefreshLogicalPhase::Attached => "Refreshing history with shared work",
            RefreshLogicalPhase::CoverageCheck => "Checking refresh coverage",
            RefreshLogicalPhase::ExactSuccessor => "Waiting for successor refresh",
            RefreshLogicalPhase::Direct => physical_label(&snapshot.progress.phase),
            RefreshLogicalPhase::Terminal => terminal_label(logical),
        },
    }
}

fn terminal_label(logical: &RefreshLogicalStatus) -> &'static str {
    logical
        .structured_outcome
        .as_ref()
        .map(|outcome| {
            if outcome.is_failure() {
                "History refresh failed"
            } else if matches!(
                outcome.code.as_str(),
                "completed" | "completed_with_rejections"
            ) {
                "History refresh complete"
            } else {
                "History refresh complete with issues"
            }
        })
        .unwrap_or("History refresh complete")
}

fn physical_label(phase: &str) -> &'static str {
    match phase {
        "queued" | "pending" | "discovering" => "Discovering history sources",
        "committing" | "committed" | "publishing" => "Publishing search index",
        "verifying" => "Verifying refreshed history",
        _ => "Indexing your agent history",
    }
}

fn source_count_text(snapshot: &RefreshProgressSnapshot) -> String {
    if snapshot.total_sources_known {
        format!(
            "{} / {}",
            format_count(snapshot.progress.completed_sources),
            format_count(
                snapshot
                    .progress
                    .total_sources
                    .max(snapshot.progress.completed_sources)
            )
        )
    } else {
        "measuring".to_owned()
    }
}

fn format_duration_millis(millis: u64) -> String {
    format_duration_seconds(millis / 1_000)
}

fn format_eta_duration_millis(millis: u64) -> String {
    format_duration_seconds(millis / 1_000 + u64::from(!millis.is_multiple_of(1_000)))
}

fn format_duration_seconds(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = minutes / 60;
    let minutes = minutes % 60;
    format!("{hours}h {minutes:02}m")
}

fn bounded_dynamic_text(value: &str) -> String {
    if value.len() <= MAX_DYNAMIC_TEXT_BYTES {
        return value.to_owned();
    }
    const SUFFIX: &str = "...";
    let mut end = MAX_DYNAMIC_TEXT_BYTES
        .saturating_sub(SUFFIX.len())
        .min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], SUFFIX)
}

#[cfg(test)]
mod tests;

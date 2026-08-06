use anyhow::Result;
use ctx_history_refresh::{
    RefreshLogicalPhase, RefreshRequestState, RefreshStatus, RefreshStatusKind,
    SourceBackedCurrentSourceProgressStage, SourceBackedRefreshProgress,
};

use crate::progress::{format_bytes, format_count};

use super::{fields, progress, Field, Progress};
use crate::ui::{Document, RenderContext};

const MAX_DYNAMIC_TEXT_BYTES: usize = 256;

/// Presentation-only view of one engine-owned status snapshot. It deliberately
/// has no wire or Serde representation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefreshProgressSnapshot {
    status: RefreshStatus,
    kind: RefreshStatusKind,
    progress: SourceBackedRefreshProgress,
    total_sources_known: bool,
}

impl RefreshProgressSnapshot {
    pub(crate) fn from_status(status: &RefreshStatus) -> Result<Self> {
        Ok(Self {
            status: status.clone(),
            kind: status.kind()?,
            progress: status.progress()?,
            total_sources_known: status.total_sources_known()?,
        })
    }

    pub(crate) fn from_schema_v1(fields: &serde_json::Value) -> Result<Self> {
        Self::from_status(&RefreshStatus::parse_schema_v1(fields.clone())?)
    }

    pub(crate) const fn status(&self) -> &RefreshStatus {
        &self.status
    }

    pub(crate) const fn kind(&self) -> &RefreshStatusKind {
        &self.kind
    }

    pub(crate) const fn progress(&self) -> &SourceBackedRefreshProgress {
        &self.progress
    }

    pub(crate) const fn total_sources_known(&self) -> bool {
        self.total_sources_known
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.kind.request_state().is_terminal()
    }

    pub(crate) fn phase(&self) -> String {
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
            .map(|current| current.stage.as_str().to_owned())
            .unwrap_or_else(|| self.progress.phase.clone())
    }

    pub(crate) fn message(&self) -> String {
        let label = refresh_label(self);
        let sources = source_count_text(self);
        match self.progress.current_source.as_deref() {
            Some(source) if !self.is_terminal() => {
                format!("{label}: {} ({sources}).", bounded_dynamic_text(source))
            }
            _ => format!("{label} ({sources})."),
        }
    }

    pub(crate) fn byte_progress(&self) -> (u64, u64) {
        let Some(current) = self.progress.current_source_progress else {
            return (0, 0);
        };
        match current.stage {
            SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            | SourceBackedCurrentSourceProgressStage::OnlineBackup => current
                .snapshot_bytes_completed
                .zip(current.snapshot_bytes_total)
                .unwrap_or((0, 0)),
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint
            | SourceBackedCurrentSourceProgressStage::LogicalScan => (0, 0),
        }
    }
}

pub(crate) fn refresh_progress(
    context: &RenderContext,
    snapshot: &RefreshProgressSnapshot,
) -> Document {
    let completed = snapshot.progress.completed_sources as u64;
    let total = snapshot
        .total_sources_known
        .then_some(snapshot.progress.total_sources as u64)
        .map(|total| total.max(completed));
    let label = refresh_label(snapshot);
    let mut document = progress(
        context,
        Progress {
            label,
            current: completed,
            total,
            detail: None,
        },
    );

    let mut details = vec![
        ("Sources", source_count_text(snapshot)),
        (
            "Logical phase",
            logical_phase_text(&snapshot.kind).to_owned(),
        ),
        (
            "Physical phase",
            humanize(&bounded_dynamic_text(&snapshot.progress.phase)),
        ),
    ];
    if let Some(source) = snapshot.progress.current_source.as_deref() {
        details.push(("Source", bounded_dynamic_text(source)));
    }
    if let Some(records) = snapshot.progress.completed_records {
        details.push(("Records", format!("{} accepted", format_count_u64(records))));
    }
    if let Some(bytes) = snapshot.progress.completed_bytes {
        details.push(("Scanned", format_bytes(bytes)));
    }
    if let RefreshStatusKind::Logical(logical) = &snapshot.kind {
        if let Some(request_id) = snapshot.status.request_id() {
            details.push(("Logical request", bounded_dynamic_text(request_id)));
        }
        details.push((
            "Physical attempt",
            bounded_dynamic_text(&logical.physical_attempt_id),
        ));
        details.push((
            "Physical state",
            request_state_text(logical.physical_attempt_state).to_owned(),
        ));
        details.push((
            "Progress owner",
            bounded_dynamic_text(&logical.progress_owner_request_id),
        ));
        details.push((
            "Owner state",
            request_state_text(logical.progress_owner_attempt_state).to_owned(),
        ));
        if let Some(outcome) = logical.structured_outcome.as_ref() {
            details.push(("Outcome", outcome.code.as_str().replace('_', " ")));
        }
    }

    let detail_fields = details
        .iter()
        .map(|(label, value)| Field::new(label, value))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(fields(context, &detail_fields));
    document
}

fn refresh_label(snapshot: &RefreshProgressSnapshot) -> &'static str {
    match &snapshot.kind {
        RefreshStatusKind::BackgroundMaintenanceWake(_) => "History refresh is queued",
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
            RefreshLogicalPhase::Terminal => logical
                .structured_outcome
                .as_ref()
                .map(|outcome| {
                    if outcome.code.is_failure() {
                        "History refresh failed"
                    } else if outcome.code.as_str() == "completed" {
                        "History refresh complete"
                    } else {
                        "History refresh complete with issues"
                    }
                })
                .unwrap_or("History refresh complete"),
        },
    }
}

fn physical_label(phase: &str) -> &'static str {
    match phase {
        "queued" | "pending" | "discovering" => "Discovering history sources",
        "committing" | "committed" | "publishing" => "Publishing search index",
        "verifying" => "Verifying refreshed history",
        _ => "Refreshing history",
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

fn logical_phase_text(kind: &RefreshStatusKind) -> &'static str {
    match kind {
        RefreshStatusKind::Legacy { .. } => "legacy",
        RefreshStatusKind::BackgroundMaintenanceWake(_) => "waiting",
        RefreshStatusKind::Logical(logical) => match logical.logical_phase {
            RefreshLogicalPhase::Waiting => "waiting",
            RefreshLogicalPhase::Attached => "attached",
            RefreshLogicalPhase::CoverageCheck => "coverage check",
            RefreshLogicalPhase::ExactSuccessor => "exact successor",
            RefreshLogicalPhase::Direct => "direct",
            RefreshLogicalPhase::Terminal => "terminal",
        },
    }
}

fn request_state_text(state: RefreshRequestState) -> &'static str {
    match state {
        RefreshRequestState::AdmissionPending => "admission pending",
        RefreshRequestState::Queued => "queued",
        RefreshRequestState::Running => "running",
        RefreshRequestState::Published => "published",
        RefreshRequestState::Failed => "failed",
    }
}

fn humanize(value: &str) -> String {
    value.replace('_', " ")
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

fn format_count_u64(value: u64) -> String {
    usize::try_from(value)
        .map(format_count)
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::ui::{StreamKind, TestContext};

    fn active_status(logical_phase: &str, physical_phase: &str, known: bool, total: u64) -> Value {
        json!({
            "request_id": "logical-request",
            "request_state": "running",
            "logical_request_id": "logical-request",
            "logical_phase": logical_phase,
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": "running",
            "progress_owner_request_id": "progress-owner",
            "progress_owner_attempt_state": "running",
            "progress": {
                "phase": physical_phase,
                "completed_sources": 0,
                "total_sources": total,
                "total_sources_known": known,
                "current_source": Value::Null,
                "completed_records": Value::Null,
                "completed_bytes": Value::Null,
                "current_source_progress": Value::Null,
            }
        })
    }

    fn terminal_status(state: &str, code: &str, class: &str) -> Value {
        json!({
            "request_id": "logical-request",
            "request_state": state,
            "logical_request_id": "logical-request",
            "logical_phase": "terminal",
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": state,
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": state,
            "structured_outcome": {
                "code": code,
                "class": class,
                "retryable": false,
                "affected_routes": [],
                "retryable_routes": [],
                "blocked_routes": [],
                "physical_attempt_id": "physical-attempt",
            },
            "progress": {
                "phase": "committed",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": true,
            }
        })
    }

    #[test]
    fn full_status_adapter_preserves_logical_phases_and_physical_owner() {
        for (phase, expected) in [
            ("waiting", "History refresh is waiting"),
            ("attached", "Refreshing history with shared work"),
            ("coverage_check", "Checking refresh coverage"),
            ("exact_successor", "Waiting for successor refresh"),
        ] {
            let snapshot = RefreshProgressSnapshot::from_schema_v1(&active_status(
                phase,
                "committed",
                true,
                2,
            ))
            .unwrap();
            assert_eq!(refresh_label(&snapshot), expected);
            assert!(!snapshot.is_terminal(), "logical phase {phase}");
            let logical = match snapshot.kind() {
                RefreshStatusKind::Logical(logical) => logical,
                other => panic!("unexpected status kind: {other:?}"),
            };
            assert_eq!(logical.physical_attempt_id, "physical-attempt");
            assert_eq!(logical.progress_owner_request_id, "progress-owner");
        }
    }

    #[test]
    fn known_zero_and_unknown_totals_remain_distinct() {
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
        let known = RefreshProgressSnapshot::from_schema_v1(&active_status(
            "direct",
            "discovering",
            true,
            0,
        ))
        .unwrap();
        let unknown = RefreshProgressSnapshot::from_schema_v1(&active_status(
            "direct",
            "discovering",
            false,
            0,
        ))
        .unwrap();
        assert!(refresh_progress(&context, &known)
            .render_plain()
            .contains("0 / 0"));
        assert!(refresh_progress(&context, &unknown)
            .render_plain()
            .contains("measuring"));
    }

    #[test]
    fn byte_progress_requires_one_complete_engine_snapshot_pair() {
        let mut status = active_status("attached", "copying", true, 2);
        status["progress"]["current_source_progress"] = json!({
            "stage": "source_family_copy",
            "snapshot_bytes_completed": 256,
            "snapshot_bytes_total": 512,
        });
        let paired = RefreshProgressSnapshot::from_schema_v1(&status).unwrap();
        assert_eq!(paired.byte_progress(), (256, 512));

        status["progress"]["current_source_progress"]
            .as_object_mut()
            .unwrap()
            .remove("snapshot_bytes_total");
        let partial = RefreshProgressSnapshot::from_schema_v1(&status).unwrap();
        assert_eq!(partial.byte_progress(), (0, 0));
    }

    #[test]
    fn structured_terminal_outcome_alone_decides_done() {
        let cases = [
            (
                "published",
                "completed",
                "completed",
                "History refresh complete",
            ),
            (
                "published",
                "completed_with_rejections",
                "completed_with_diagnostics",
                "History refresh complete with issues",
            ),
            (
                "failed",
                "source_refresh_failed",
                "internal",
                "History refresh failed",
            ),
        ];
        for (state, code, class, label) in cases {
            let snapshot =
                RefreshProgressSnapshot::from_schema_v1(&terminal_status(state, code, class))
                    .unwrap();
            assert!(snapshot.is_terminal());
            assert_eq!(refresh_label(&snapshot), label);
        }

        let physically_committed =
            RefreshProgressSnapshot::from_schema_v1(&active_status("direct", "committed", true, 0))
                .unwrap();
        assert!(!physically_committed.is_terminal());
    }
}

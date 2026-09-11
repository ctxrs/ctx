use super::*;
use ctx_history_refresh::{
    RefreshStatusKind, SourceBackedRefreshProgress, SourceBackedRefreshStage,
};

// Bound foreground observation, not the worker's lifetime. Large refreshes can
// continue for any duration while publishing progress. A quiet worker may also
// finish later; lack of observable progress does not prove it failed or died.
const NO_PROGRESS_BUDGET: StdDuration = StdDuration::from_secs(5 * 60);

#[derive(Default)]
pub(super) struct ProgressDeadline {
    last: Option<(
        RefreshStatusKind,
        SourceBackedRefreshProgress,
        SourceBackedRefreshStage,
        StdInstant,
    )>,
}

impl ProgressDeadline {
    pub(super) fn observe(&mut self, status: &RefreshStatus, now: StdInstant) -> Result<()> {
        let kind = status.kind()?;
        if kind.request_state().is_terminal() {
            return Ok(());
        }
        let mut progress = status.progress()?;
        // UI clocks and estimates change even if the work does not.
        progress.elapsed_millis = None;
        let stage = status.whole_run_stage()?;
        if let Some((last_kind, last_progress, last_stage, changed_at)) = self.last.as_ref() {
            if *last_kind == kind && *last_progress == progress && *last_stage == stage {
                let quiet_for = now.saturating_duration_since(*changed_at);
                if quiet_for >= NO_PROGRESS_BUDGET {
                    return Err(SourceRefreshNoProgress {
                        request_id: status
                            .request_id()
                            .context("refresh progress has no request ID")?
                            .to_owned(),
                        state: kind.request_state(),
                        quiet_for,
                    }
                    .into());
                }
                return Ok(());
            }
        }
        self.last = Some((kind, progress, stage, now));
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct SourceRefreshNoProgress {
    pub(super) request_id: String,
    pub(super) state: RefreshRequestState,
    pub(super) quiet_for: StdDuration,
}

impl fmt::Display for SourceRefreshNoProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter,
            "daemon source refresh request {} is {} with no observable progress for {} seconds; outcome is unknown; the admitted request is retained and may finish later; inspect `ctx daemon status` and `ctx index` before retrying",
            self.request_id, self.state.as_str(), self.quiet_for.as_secs())
    }
}

impl std::error::Error for SourceRefreshNoProgress {}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: &str, elapsed: u64, completed: u64) -> RefreshStatus {
        RefreshStatus::parse_schema_v1(json!({
            "request_id": "progress-budget-test",
            "request_state": state,
            "progress": {
                "phase": "scanning", "completed_sources": 0, "total_sources": 1,
                "completed_bytes": completed,
                "elapsed_millis": elapsed,
                "estimated_remaining_millis": 1000_u64.saturating_sub(elapsed),
            }
        }))
        .unwrap()
    }

    #[test]
    fn elapsed_time_and_estimates_do_not_extend_nonterminal_waits() {
        let start = StdInstant::now();
        for state in ["admission_pending", "queued", "running"] {
            let mut deadline = ProgressDeadline::default();
            deadline.observe(&status(state, 0, 0), start).unwrap();
            deadline
                .observe(
                    &status(state, 299_999, 0),
                    start + StdDuration::from_millis(299_999),
                )
                .unwrap();
            let error = deadline
                .observe(
                    &status(state, 300_000, 0),
                    start + StdDuration::from_secs(300),
                )
                .unwrap_err();
            let stalled = error.downcast_ref::<SourceRefreshNoProgress>().unwrap();
            assert_eq!(stalled.request_id, "progress-budget-test");
            assert_eq!(stalled.state.as_str(), state);
            assert_eq!(stalled.quiet_for.as_secs(), 300);
        }
    }

    #[test]
    fn slow_advancing_work_can_run_beyond_the_budget() {
        let start = StdInstant::now();
        let mut deadline = ProgressDeadline::default();
        for step in 0..20 {
            deadline
                .observe(
                    &status("running", step * 240_000, step),
                    start + StdDuration::from_secs(step * 240),
                )
                .unwrap();
        }
    }

    #[test]
    fn state_and_stage_changes_restart_observation_but_terminal_results_win() {
        let start = StdInstant::now();
        let mut deadline = ProgressDeadline::default();
        deadline.observe(&status("queued", 0, 0), start).unwrap();
        deadline
            .observe(
                &status("running", 300_000, 0),
                start + StdDuration::from_secs(300),
            )
            .unwrap();
        let mut fields = status("running", 600_000, 0).schema_v1_fields().clone();
        fields["progress"]["whole_run_stage"] = json!("merging");
        deadline
            .observe(
                &RefreshStatus::parse_schema_v1(fields).unwrap(),
                start + StdDuration::from_secs(600),
            )
            .unwrap();
        for state in ["published", "failed"] {
            deadline
                .observe(
                    &status(state, 900_000, 0),
                    start + StdDuration::from_secs(900),
                )
                .unwrap();
        }
    }
}

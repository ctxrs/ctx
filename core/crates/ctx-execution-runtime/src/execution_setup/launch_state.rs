use super::progress::{estimate_remaining_ms, format_ts, project_progress_pct};
use super::*;

#[derive(Debug, Default)]
pub(super) struct CoordinatorState {
    pub(super) launch_jobs: HashMap<String, Arc<LaunchJob>>,
    pub(super) launch_history: VecDeque<String>,
    pub(super) running_launch_by_workspace: HashMap<WorkspaceId, String>,
    pub(super) prewarm_jobs: PrewarmJobRegistry,
    pub(super) startup: StartupPrewarmSnapshot,
}

impl CoordinatorState {
    pub(super) fn remember_launch_job(&mut self, job: Arc<LaunchJob>) {
        self.launch_jobs
            .insert(job.job_id.clone(), Arc::clone(&job));
        self.launch_history.push_back(job.job_id.clone());
        while self.launch_history.len() > JOB_HISTORY_CAP {
            let Some(old_id) = self.launch_history.pop_front() else {
                break;
            };
            if !self.is_job_running(&old_id) {
                self.launch_jobs.remove(&old_id);
            }
        }
    }

    fn is_job_running(&self, job_id: &str) -> bool {
        self.running_launch_by_workspace
            .values()
            .any(|active_id| active_id == job_id)
            || self.prewarm_jobs.contains_job_id(job_id)
    }
}

#[derive(Debug)]
pub(super) struct LaunchPhaseRecord {
    pub(super) phase: HarnessSetupPhase,
    pub(super) started_at: DateTime<Utc>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) elapsed_ms: Option<u64>,
}

#[derive(Debug)]
pub(super) struct LaunchJobInner {
    pub(super) kind: ExecutionSetupJobKind,
    pub(super) state: ExecutionLaunchState,
    pub(super) created_at: DateTime<Utc>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) current_phase: Option<HarnessSetupPhase>,
    pub(super) current_step_label: Option<String>,
    pub(super) active_download: Option<HarnessSetupDownloadStatus>,
    pub(super) phases: Vec<LaunchPhaseRecord>,
    pub(super) logs: VecDeque<ExecutionLaunchLogLine>,
    pub(super) next_seq: u64,
    pub(super) error: Option<String>,
}

impl LaunchJobInner {
    pub(super) fn new(kind: ExecutionSetupJobKind) -> Self {
        let now = Utc::now();
        Self {
            kind,
            state: ExecutionLaunchState::Running,
            created_at: now,
            started_at: now,
            finished_at: None,
            current_phase: None,
            current_step_label: None,
            active_download: None,
            phases: Vec::new(),
            logs: VecDeque::new(),
            next_seq: 0,
            error: None,
        }
    }

    pub(super) fn push_log(
        &mut self,
        phase: HarnessSetupPhase,
        level: HarnessSetupLogLevel,
        message: &str,
        now: DateTime<Utc>,
    ) -> ExecutionLaunchLogLine {
        self.next_seq += 1;
        let line = ExecutionLaunchLogLine {
            seq: self.next_seq,
            ts: format_ts(now),
            phase,
            level,
            message: message.to_string(),
        };
        self.logs.push_back(line.clone());
        while self.logs.len() > JOB_LOG_CAP {
            self.logs.pop_front();
        }
        line
    }

    fn close_current_phase(&mut self, now: DateTime<Utc>) -> Option<CompletedPhase> {
        let phase = self.current_phase?;
        let Some(last) = self.phases.last_mut() else {
            self.current_phase = None;
            return None;
        };
        if last.phase != phase || last.finished_at.is_some() {
            self.current_phase = None;
            return None;
        }
        last.finished_at = Some(now);
        let elapsed = now.signed_duration_since(last.started_at);
        let elapsed_ms = elapsed.num_milliseconds().max(0) as u64;
        last.elapsed_ms = Some(elapsed_ms);
        self.current_phase = None;
        Some(CompletedPhase { phase, elapsed_ms })
    }

    fn snapshot(&self, job_id: &str, workspace_id: WorkspaceId) -> ExecutionLaunchSnapshot {
        let now = Utc::now();
        let eta_ms = estimate_remaining_ms(self, now);
        let progress_pct = project_progress_pct(self, now, eta_ms);
        ExecutionLaunchSnapshot {
            job_id: job_id.to_string(),
            workspace_id: workspace_id.0.to_string(),
            kind: self.kind,
            state: self.state,
            created_at: format_ts(self.created_at),
            started_at: format_ts(self.started_at),
            updated_at: format_ts(now),
            finished_at: self.finished_at.map(format_ts),
            current_phase: self.current_phase,
            current_step_label: self.current_step_label.clone(),
            progress_pct,
            eta_ms,
            active_download: self.active_download.clone(),
            phases: self
                .phases
                .iter()
                .map(|phase| ExecutionLaunchPhaseStatus {
                    phase: phase.phase,
                    started_at: format_ts(phase.started_at),
                    finished_at: phase.finished_at.map(format_ts),
                    elapsed_ms: phase.elapsed_ms,
                })
                .collect(),
            logs: self.logs.iter().cloned().collect(),
            error: self.error.clone(),
        }
    }
}

#[derive(Debug)]
pub(super) struct LaunchJob {
    pub(super) job_id: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) tx: broadcast::Sender<ExecutionLaunchStreamEvent>,
    pub(super) inner: StdMutex<LaunchJobInner>,
}

impl LaunchJob {
    pub(super) fn new(job_id: String, workspace_id: WorkspaceId) -> Self {
        Self::new_with_kind(job_id, workspace_id, ExecutionSetupJobKind::WorkspaceLaunch)
    }

    pub(super) fn new_with_kind(
        job_id: String,
        workspace_id: WorkspaceId,
        kind: ExecutionSetupJobKind,
    ) -> Self {
        let (tx, _) = broadcast::channel(LAUNCH_EVENT_CHANNEL_CAP);
        Self {
            job_id,
            workspace_id,
            tx,
            inner: StdMutex::new(LaunchJobInner::new(kind)),
        }
    }

    pub(super) fn snapshot(&self) -> ExecutionLaunchSnapshot {
        let inner = lock_or_recover(&self.inner, "launch job");
        inner.snapshot(&self.job_id, self.workspace_id)
    }

    pub(super) fn current_phase(&self) -> Option<HarnessSetupPhase> {
        let inner = lock_or_recover(&self.inner, "launch job");
        inner.current_phase
    }

    pub(super) fn is_terminal(&self) -> bool {
        let inner = lock_or_recover(&self.inner, "launch job");
        inner.state != ExecutionLaunchState::Running
    }

    pub(super) fn transition_phase(
        &self,
        phase: HarnessSetupPhase,
        message: &str,
    ) -> LaunchMutation {
        let now = Utc::now();
        let mut inner = lock_or_recover(&self.inner, "launch job");
        let mut completed_phase = None;
        let mut snapshot_changed = false;
        let previous_phase = inner.current_phase;
        let previous_label = inner.current_step_label.clone();
        if inner.current_phase != Some(phase) {
            completed_phase = inner.close_current_phase(now);
            inner.current_phase = Some(phase);
            if phase != HarnessSetupPhase::ArtifactDownload {
                inner.active_download = None;
            }
            inner.phases.push(LaunchPhaseRecord {
                phase,
                started_at: now,
                finished_at: None,
                elapsed_ms: None,
            });
            snapshot_changed = true;
        }
        let next_label = message.trim();
        let label = if next_label.is_empty() {
            None
        } else {
            Some(next_label.to_string())
        };
        if inner.current_step_label != label {
            inner.current_step_label = label;
            snapshot_changed = true;
        }
        let should_log = previous_phase != Some(phase)
            || previous_label.as_deref().map(str::trim) != Some(next_label);
        let line = if should_log {
            Some(inner.push_log(phase, HarnessSetupLogLevel::Info, message, now))
        } else {
            None
        };
        let snapshot = inner.snapshot(&self.job_id, self.workspace_id);
        LaunchMutation {
            line,
            snapshot,
            snapshot_changed,
            completed_phase,
        }
    }

    pub(super) fn push_log(
        &self,
        phase: HarnessSetupPhase,
        level: HarnessSetupLogLevel,
        message: &str,
    ) -> LaunchMutation {
        let now = Utc::now();
        let mut inner = lock_or_recover(&self.inner, "launch job");
        let line = inner.push_log(phase, level, message, now);
        let snapshot = inner.snapshot(&self.job_id, self.workspace_id);
        LaunchMutation {
            line: Some(line),
            snapshot,
            snapshot_changed: false,
            completed_phase: None,
        }
    }

    pub(super) fn set_progress(&self, progress: HarnessSetupProgressUpdate) -> LaunchMutation {
        let mut inner = lock_or_recover(&self.inner, "launch job");
        inner.active_download = progress.active_download;
        let snapshot = inner.snapshot(&self.job_id, self.workspace_id);
        LaunchMutation {
            line: None,
            snapshot,
            snapshot_changed: true,
            completed_phase: None,
        }
    }

    pub(super) fn mark_terminal(
        &self,
        state: ExecutionLaunchState,
        error: Option<String>,
    ) -> LaunchTerminalMutation {
        let now = Utc::now();
        let mut inner = lock_or_recover(&self.inner, "launch job");
        let completed_phase = inner.close_current_phase(now);
        inner.state = state;
        inner.finished_at = Some(now);
        inner.error = error;
        inner.active_download = None;
        let snapshot = inner.snapshot(&self.job_id, self.workspace_id);
        LaunchTerminalMutation {
            snapshot,
            completed_phase,
        }
    }
}

#[derive(Debug)]
pub(super) struct CompletedPhase {
    pub(super) phase: HarnessSetupPhase,
    pub(super) elapsed_ms: u64,
}

#[derive(Debug)]
pub(super) struct LaunchMutation {
    pub(super) line: Option<ExecutionLaunchLogLine>,
    pub(super) snapshot: ExecutionLaunchSnapshot,
    pub(super) snapshot_changed: bool,
    pub(super) completed_phase: Option<CompletedPhase>,
}

#[derive(Debug)]
pub(super) struct LaunchTerminalMutation {
    pub(super) snapshot: ExecutionLaunchSnapshot,
    pub(super) completed_phase: Option<CompletedPhase>,
}

pub(super) fn seed_workspace_launch_initial_state(job: &LaunchJob, settings: &ExecutionSettings) {
    if matches!(settings.mode, ExecutionMode::Host) {
        let _ = job.transition_phase(
            HarnessSetupPhase::Ready,
            "host execution mode selected; container launch skipped",
        );
    } else {
        let _ = job.transition_phase(
            HarnessSetupPhase::MachineCheck,
            "requesting shared container readiness",
        );
    }
}

pub(super) fn seed_runtime_prewarm_initial_state(job: &LaunchJob, settings: &ExecutionSettings) {
    if matches!(settings.mode, ExecutionMode::Host) {
        let _ = job.transition_phase(
            HarnessSetupPhase::Ready,
            "host execution mode selected; runtime prewarm skipped",
        );
    } else {
        let _ = job.transition_phase(
            HarnessSetupPhase::MachineCheck,
            "requesting shared container readiness",
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StartupPrewarmMetadata {
    pub(super) image_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) bundled_image_fingerprint: Option<String>,
    pub(super) ready_at: String,
}

#[derive(Debug)]
pub(super) struct PrewarmGate {
    pub(super) machine_ready: bool,
    pub(super) image_present: bool,
    pub(super) image_ref_changed: bool,
    pub(super) bundled_image_digest_changed: bool,
    pub(super) needs_prewarm: bool,
    pub(super) bundled_image_fingerprint: Option<String>,
}

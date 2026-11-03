use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ctx_history_core::{CaptureProvider, CoreRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRefreshProgress {
    pub phase: &'static str,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
    /// Core records accepted for the active source route. No total is implied.
    pub completed_records: Option<u64>,
    /// Authoritative logical source bytes completed for the active route. No total is implied.
    pub completed_bytes: Option<u64>,
    /// Stable provider identities represented by the selected executable routes.
    pub providers: Vec<CaptureProvider>,
    /// Distinct normalized sessions observed across this refresh attempt.
    pub processed_sessions: u64,
    /// Normalized message records observed across this refresh attempt.
    pub processed_messages: u64,
    /// Normalized tool-call records observed across this refresh attempt.
    pub processed_tool_calls: u64,
    /// Logical input bytes processed across this refresh attempt.
    pub processed_bytes: u64,
    /// Time spent in the current phase when this event was emitted.
    pub stage_duration: Duration,
    /// Total measured discovery plus refresh time at this event.
    pub elapsed: Duration,
    /// Commit-derived source evidence, available only after publication.
    pub certified_source_count: Option<usize>,
    /// Commit-derived byte evidence, available only after publication.
    pub certified_source_bytes: Option<u64>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "discovering",
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            providers: Vec::new(),
            processed_sessions: 0,
            processed_messages: 0,
            processed_tool_calls: 0,
            processed_bytes: 0,
            stage_duration: Duration::ZERO,
            elapsed: Duration::ZERO,
            certified_source_count: None,
            certified_source_bytes: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
    /// Transient evidence that a source scanner is consuming physical input.
    Parsing,
    /// Transient evidence that accepted Core candidates are waiting on or
    /// being applied to the index writer.
    IndexWriting,
}

impl SourceBackedCurrentSourceProgressStage {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBackedCurrentSourceProgress {
    pub stage: SourceBackedCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl SourceBackedCurrentSourceProgress {
    pub const fn new(stage: SourceBackedCurrentSourceProgressStage) -> Self {
        Self {
            stage,
            snapshot_pages_completed: None,
            snapshot_pages_total: None,
            snapshot_bytes_completed: None,
            snapshot_bytes_total: None,
            logical_rows_scanned: None,
            logical_certified_bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedDetailedRefreshProgress {
    pub progress: SourceBackedRefreshProgress,
    pub current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    /// Attempt-local estimator input. This is never part of public progress.
    #[doc(hidden)]
    pub exact_scan_progress: Option<SourceBackedExactScanProgress>,
}

/// Exact logical bytes from the selected routes' existing scan accounting.
///
/// This is an internal model input, not a source-scan progress contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBackedExactScanProgress {
    pub total_bytes: u64,
    pub completed_bytes: u64,
}

impl SourceBackedDetailedRefreshProgress {
    pub fn into_legacy(self) -> SourceBackedRefreshProgress {
        self.progress
    }
}

pub fn source_level_progress(
    progress: SourceBackedRefreshProgress,
) -> SourceBackedDetailedRefreshProgress {
    SourceBackedDetailedRefreshProgress {
        progress,
        current_source_progress: None,
        exact_scan_progress: None,
    }
}

#[derive(Debug, Default)]
pub struct SourceRecordProgress {
    pub completed_records: u64,
    pub completed_bytes: u64,
    last_emitted_records: u64,
    last_emitted_bytes: u64,
    last_emitted_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRecordProgressSnapshot {
    pub completed_records: u64,
    pub completed_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceBackedRecordProgressDelta {
    pub accepted_records: u64,
    pub completed_bytes: u64,
    /// One exact route total from accounting already performed by its scanner.
    #[doc(hidden)]
    pub exact_total_bytes: Option<u64>,
    /// Exact physical bytes advanced by this existing scanner callback.
    #[doc(hidden)]
    pub exact_completed_bytes: Option<u64>,
    pub session_ids: Vec<[u8; 32]>,
    pub messages: u64,
    pub tool_calls: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreRecordProgress {
    pub session_id: [u8; 32],
    pub messages: u64,
    pub tool_calls: u64,
}

impl CoreRecordProgress {
    pub fn from_record(record: &CoreRecord) -> Self {
        Self {
            session_id: record.session_id.digest(),
            messages: u64::from(record.event_type == "message"),
            tool_calls: u64::from(record.event_type == "tool_call"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CoreRecordBatchProgress {
    pub session_ids: Vec<[u8; 32]>,
    pub messages: u64,
    pub tool_calls: u64,
}

impl CoreRecordBatchProgress {
    pub fn push(&mut self, progress: CoreRecordProgress) {
        if self.session_ids.last() != Some(&progress.session_id) {
            self.session_ids.push(progress.session_id);
        }
        self.messages = self.messages.saturating_add(progress.messages);
        self.tool_calls = self.tool_calls.saturating_add(progress.tool_calls);
    }
}

#[derive(Debug, Default)]
pub struct AttemptHistoryProgress {
    processed_session_ids: HashSet<[u8; 32]>,
    processed_messages: u64,
    processed_tool_calls: u64,
    processed_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttemptHistoryProgressSnapshot {
    pub processed_sessions: u64,
    pub processed_messages: u64,
    pub processed_tool_calls: u64,
    /// Completed logical source coverage, including retained certified bytes.
    /// This is progress accounting, not bytes read from sources or index files.
    pub processed_bytes: u64,
}

impl AttemptHistoryProgress {
    pub fn advance(&mut self, delta: &SourceBackedRecordProgressDelta) {
        self.processed_session_ids
            .extend(delta.session_ids.iter().copied());
        self.processed_messages = self.processed_messages.saturating_add(delta.messages);
        self.processed_tool_calls = self.processed_tool_calls.saturating_add(delta.tool_calls);
        self.processed_bytes = self.processed_bytes.saturating_add(delta.completed_bytes);
    }

    pub fn snapshot(&self) -> AttemptHistoryProgressSnapshot {
        AttemptHistoryProgressSnapshot {
            processed_sessions: u64::try_from(self.processed_session_ids.len()).unwrap_or(u64::MAX),
            processed_messages: self.processed_messages,
            processed_tool_calls: self.processed_tool_calls,
            processed_bytes: self.processed_bytes,
        }
    }
}

/// Attempt-local live history facts shared by parallel scanners and the
/// coordinator. This is intentionally transient: it makes already-prepared
/// page facts observable while the coordinator is blocked, but the terminal
/// publication receipt remains the durable authority.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct SharedAttemptHistoryProgress {
    state: Arc<Mutex<SharedAttemptHistoryProgressState>>,
}

#[derive(Debug, Default)]
struct SharedAttemptHistoryProgressState {
    history: AttemptHistoryProgress,
    parallel_byte_debt: u64,
}

impl SharedAttemptHistoryProgress {
    pub fn snapshot(&self) -> AttemptHistoryProgressSnapshot {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .history
            .snapshot()
    }

    /// Publishes exact scanner-owned page facts before the worker rendezvous.
    /// The coordinator later consumes the byte debt when it reports the same
    /// source-local progress, so the shared attempt total is never summed.
    pub fn publish_parallel_page(&self, completed_bytes: u64, progress: &CoreRecordBatchProgress) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .history
            .processed_session_ids
            .extend(progress.session_ids.iter().copied());
        state.history.processed_messages = state
            .history
            .processed_messages
            .saturating_add(progress.messages);
        state.history.processed_tool_calls = state
            .history
            .processed_tool_calls
            .saturating_add(progress.tool_calls);
        state.history.processed_bytes = state
            .history
            .processed_bytes
            .saturating_add(completed_bytes);
        state.parallel_byte_debt = state.parallel_byte_debt.saturating_add(completed_bytes);
    }

    /// Incorporates coordinator-owned progress, consuming bytes that were
    /// already published by a parallel worker first. Session/message/tool
    /// facts are supplied only by serial coordinator paths.
    pub fn advance_coordinator(&self, delta: &SourceBackedRecordProgressDelta) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let covered = state.parallel_byte_debt.min(delta.completed_bytes);
        state.parallel_byte_debt -= covered;
        state
            .history
            .processed_session_ids
            .extend(delta.session_ids.iter().copied());
        state.history.processed_messages = state
            .history
            .processed_messages
            .saturating_add(delta.messages);
        state.history.processed_tool_calls = state
            .history
            .processed_tool_calls
            .saturating_add(delta.tool_calls);
        state.history.processed_bytes = state
            .history
            .processed_bytes
            .saturating_add(delta.completed_bytes.saturating_sub(covered));
    }

    /// A new route must not consume a prior route's parallel byte debt.
    pub fn reset_parallel_byte_debt(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .parallel_byte_debt = 0;
    }

    pub fn parallel_byte_debt(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .parallel_byte_debt
    }
}

impl SourceRecordProgress {
    pub fn advanced_at(
        &mut self,
        delta: SourceBackedRecordProgressDelta,
        now: Instant,
        minimum_emit_interval: Duration,
    ) -> Option<SourceRecordProgressSnapshot> {
        self.completed_records = self
            .completed_records
            .saturating_add(delta.accepted_records);
        self.completed_bytes = self.completed_bytes.saturating_add(delta.completed_bytes);
        let should_emit = self
            .last_emitted_at
            .is_none_or(|last| now.saturating_duration_since(last) >= minimum_emit_interval);
        should_emit.then(|| self.mark_emitted(now))
    }

    pub fn flush_at(&mut self, now: Instant) -> Option<SourceRecordProgressSnapshot> {
        (self.completed_records != self.last_emitted_records
            || self.completed_bytes != self.last_emitted_bytes)
            .then(|| self.mark_emitted(now))
    }

    fn mark_emitted(&mut self, now: Instant) -> SourceRecordProgressSnapshot {
        self.last_emitted_at = Some(now);
        self.last_emitted_records = self.completed_records;
        self.last_emitted_bytes = self.completed_bytes;
        self.snapshot()
    }

    pub fn snapshot(&self) -> SourceRecordProgressSnapshot {
        SourceRecordProgressSnapshot {
            completed_records: self.completed_records,
            completed_bytes: self.completed_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_RECORD_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

    #[test]
    fn shared_attempt_progress_deduplicates_workers_and_consumes_parallel_byte_debt() {
        let shared = SharedAttemptHistoryProgress::default();
        let first = CoreRecordBatchProgress {
            session_ids: vec![[7; 32]],
            messages: 2,
            tool_calls: 1,
        };
        let duplicate = CoreRecordBatchProgress {
            session_ids: vec![[7; 32]],
            messages: 3,
            tool_calls: 2,
        };
        shared.publish_parallel_page(40, &first);
        shared.publish_parallel_page(60, &duplicate);
        assert_eq!(
            shared.snapshot(),
            AttemptHistoryProgressSnapshot {
                processed_sessions: 1,
                processed_messages: 5,
                processed_tool_calls: 3,
                processed_bytes: 100,
            }
        );
        shared.advance_coordinator(&SourceBackedRecordProgressDelta {
            completed_bytes: 100,
            ..Default::default()
        });
        assert_eq!(shared.parallel_byte_debt(), 0);
        assert_eq!(shared.snapshot().processed_bytes, 100);
    }

    #[test]
    fn source_record_progress_is_prompt_throttled_monotonic_and_flushable() {
        let started = Instant::now();
        let mut progress = SourceRecordProgress::default();
        let accepted = SourceBackedRecordProgressDelta {
            accepted_records: 1,
            ..Default::default()
        };
        let bytes = SourceBackedRecordProgressDelta {
            completed_bytes: 512,
            ..Default::default()
        };

        assert_eq!(
            progress.advanced_at(bytes.clone(), started, SOURCE_RECORD_PROGRESS_INTERVAL),
            Some(SourceRecordProgressSnapshot {
                completed_records: 0,
                completed_bytes: 512,
            })
        );
        assert_eq!(
            progress.advanced_at(
                accepted.clone(),
                started + Duration::from_millis(500),
                SOURCE_RECORD_PROGRESS_INTERVAL,
            ),
            None
        );
        assert_eq!(
            progress.advanced_at(
                bytes,
                started + SOURCE_RECORD_PROGRESS_INTERVAL,
                SOURCE_RECORD_PROGRESS_INTERVAL,
            ),
            Some(SourceRecordProgressSnapshot {
                completed_records: 1,
                completed_bytes: 1_024,
            })
        );
        assert_eq!(
            progress.advanced_at(
                accepted.clone(),
                started + Duration::from_millis(1_100),
                SOURCE_RECORD_PROGRESS_INTERVAL,
            ),
            None
        );
        assert_eq!(
            progress.flush_at(started + Duration::from_millis(1_100)),
            Some(SourceRecordProgressSnapshot {
                completed_records: 2,
                completed_bytes: 1_024,
            })
        );
        assert_eq!(
            progress.flush_at(started + Duration::from_millis(1_100)),
            None
        );

        let mut next_source = SourceRecordProgress::default();
        assert_eq!(next_source.completed_records, 0);
        assert_eq!(next_source.completed_bytes, 0);
        assert_eq!(
            next_source.advanced_at(accepted, started, SOURCE_RECORD_PROGRESS_INTERVAL),
            Some(SourceRecordProgressSnapshot {
                completed_records: 1,
                completed_bytes: 0,
            })
        );
        assert_eq!(next_source.flush_at(started), None);
    }

    #[test]
    fn attempt_history_progress_deduplicates_full_session_identity_and_accumulates_counts() {
        let first_session = [0x11; 32];
        let second_session = [0x22; 32];
        let mut progress = AttemptHistoryProgress::default();
        progress.advance(&SourceBackedRecordProgressDelta {
            accepted_records: 4,
            completed_bytes: 1_024,
            exact_total_bytes: None,
            exact_completed_bytes: None,
            session_ids: vec![first_session, second_session, first_session],
            messages: 3,
            tool_calls: 1,
        });
        progress.advance(&SourceBackedRecordProgressDelta {
            accepted_records: 2,
            completed_bytes: 512,
            exact_total_bytes: None,
            exact_completed_bytes: None,
            session_ids: vec![second_session],
            messages: 1,
            tool_calls: 1,
        });

        assert_eq!(
            progress.snapshot(),
            AttemptHistoryProgressSnapshot {
                processed_sessions: 2,
                processed_messages: 4,
                processed_tool_calls: 2,
                processed_bytes: 1_536,
            }
        );
    }

    #[test]
    fn exact_total_declaration_does_not_change_ingestion_progress() {
        let delta = SourceBackedRecordProgressDelta {
            exact_total_bytes: Some(4_096),
            ..Default::default()
        };
        let mut attempt = AttemptHistoryProgress::default();
        attempt.advance(&delta);
        assert_eq!(
            attempt.snapshot(),
            AttemptHistoryProgressSnapshot::default()
        );

        let mut source = SourceRecordProgress::default();
        assert_eq!(
            source.advanced_at(delta, Instant::now(), SOURCE_RECORD_PROGRESS_INTERVAL),
            Some(SourceRecordProgressSnapshot {
                completed_records: 0,
                completed_bytes: 0,
            })
        );
    }
}

use std::time::Duration;

use ctx_history_sharing as source;

use super::{counts, histogram, measured, wire};

pub(super) fn project(observation: source::SharingObservation) -> wire::SharingSummaryV1 {
    use source::SharingObservation as O;
    use wire::SharingOperation as Op;
    let operation = match observation {
        O::WorkerStarted => Op::WorkerStarted,
        O::WorkerStartFailed(_) => Op::WorkerStartFailed,
        O::WorkerStopped => Op::WorkerStopped,
        O::Tick { .. } => Op::Tick,
        O::Selection { .. } => Op::Selection,
        O::Queued { .. } => Op::Queued,
        O::Transfer { .. } => Op::Transfer,
        O::Accepted { .. } => Op::Accepted,
        O::Settled { .. } => Op::Settled,
        O::Retry { .. } => Op::Retry,
    };
    let mut summary = wire::SharingSummaryV1::new(operation, Duration::ZERO);
    summary.counts = counts(None, false);
    match observation {
        O::WorkerStarted | O::WorkerStopped => {}
        O::WorkerStartFailed(value) => {
            summary.counts.failed = 1;
            summary.failure = Some(failure(value));
        }
        O::Tick {
            phase: p,
            outcome,
            duration,
            failure: f,
            progress_after_failure,
        } => {
            summary.phase = Some(phase(p));
            summary.tick = Some(tick(outcome));
            summary.failure = f.map(failure);
            summary.counts = counts(Some(duration), outcome == source::SharingTick::Failed);
            summary.progress_after_failure = Some(u64::from(progress_after_failure));
        }
        O::Selection {
            decision,
            count,
            complete,
        } => {
            summary.selection = Some(selection(decision));
            summary.selected_count = Some(count);
            summary.selection_complete = Some(complete);
        }
        O::Queued { bytes, records } => {
            summary.bytes = Some(measured(bytes));
            summary.records = Some(measured(records));
        }
        O::Transfer { bytes } => summary.bytes = Some(measured(bytes)),
        O::Accepted {
            bytes,
            records,
            recovered_receipt,
        } => {
            summary.bytes = Some(measured(bytes));
            summary.records = Some(measured(records));
            summary.recovered_receipts = Some(u64::from(recovered_receipt));
        }
        O::Settled { already_accepted } => {
            summary.already_accepted = Some(u64::from(already_accepted))
        }
        O::Retry {
            phase: p,
            failure: f,
            attempts,
            delay,
        } => {
            summary.phase = Some(phase(p));
            summary.failure = Some(failure(f));
            summary.counts.failed = 1;
            summary.retry_attempts = Some(measured(u64::from(attempts)));
            summary.retry_delay = Some(histogram(delay));
        }
    }
    summary
}

fn failure(value: source::SharingFailure) -> wire::SharingFailure {
    use source::SharingFailure as S;
    use wire::SharingFailure as W;
    match value {
        S::Configuration => W::Configuration,
        S::Credentials => W::Credentials,
        S::State => W::State,
        S::NotConnected => W::NotConnected,
        S::DestinationChanged => W::DestinationChanged,
        S::PolicyConflict => W::PolicyConflict,
        S::PolicyDenied => W::PolicyDenied,
        S::Busy => W::Busy,
        S::Unavailable => W::Unavailable,
        S::Unauthorized => W::Unauthorized,
        S::Forbidden => W::Forbidden,
        S::NotFound => W::NotFound,
        S::Conflict => W::Conflict,
        S::StagingExpired => W::StagingExpired,
        S::TooLarge => W::TooLarge,
        S::RateLimited => W::RateLimited,
        S::Protocol => W::Protocol,
        S::HttpRejected => W::HttpRejected,
        S::Archive => W::Archive,
    }
}

fn phase(value: source::SharingPhase) -> wire::SharingPhase {
    use source::SharingPhase as S;
    use wire::SharingPhase as W;
    match value {
        S::Settings => W::Settings,
        S::Admission => W::Admission,
        S::Capture => W::Capture,
        S::BeginUpload => W::BeginUpload,
        S::UploadStatus => W::UploadStatus,
        S::UploadChunk => W::UploadChunk,
        S::Publish => W::Publish,
        S::Receipt => W::Receipt,
        S::Settlement => W::Settlement,
        S::Checkpoint => W::Checkpoint,
    }
}

fn tick(value: source::SharingTick) -> wire::SharingTick {
    match value {
        source::SharingTick::Disabled => wire::SharingTick::Disabled,
        source::SharingTick::Paused => wire::SharingTick::Paused,
        source::SharingTick::Idle => wire::SharingTick::Idle,
        source::SharingTick::Progress => wire::SharingTick::Progress,
        source::SharingTick::Failed => wire::SharingTick::Failed,
    }
}

fn selection(value: source::SelectionDecision) -> wire::SharingSelection {
    match value {
        source::SelectionDecision::Selected => wire::SharingSelection::Selected,
        source::SelectionDecision::UnselectedSource => wire::SharingSelection::UnselectedSource,
        source::SelectionDecision::ChangedProfile => wire::SharingSelection::ChangedProfile,
        source::SelectionDecision::OutsideWorkRoots => wire::SharingSelection::OutsideWorkRoots,
        source::SelectionDecision::UnknownWorkRoot => wire::SharingSelection::UnknownWorkRoot,
        source::SelectionDecision::BackfillExcluded => wire::SharingSelection::BackfillExcluded,
        source::SelectionDecision::FutureExcluded => wire::SharingSelection::FutureExcluded,
        source::SelectionDecision::NeedsReview => wire::SharingSelection::NeedsReview,
    }
}

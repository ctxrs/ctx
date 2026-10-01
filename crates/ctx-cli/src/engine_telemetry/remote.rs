use crate::remote_history as native;
use ctx_client_observability::analytics as wire;

pub(crate) fn remote(terminal: native::RemoteCompletion, mcp: bool) -> wire::RemoteCompletedV1 {
    let execution = match terminal.failure {
        None => Ok(()),
        // Writer failure does not rewrite completed remote work as failed work.
        // A failed session prefix can prevent the first request entirely.
        Some(native::RemoteFailure::Output) if terminal.facts.requests > 0 => Ok(()),
        Some(failure) => Err(project_failure(failure, terminal.stage)),
    };
    let delivery = match terminal.facts.output_flushed {
        Some(true) => wire::DeliveryEvidence::KnownComplete,
        Some(false) => wire::DeliveryEvidence::Failed,
        None if terminal.failure == Some(native::RemoteFailure::Output) => {
            wire::DeliveryEvidence::Failed
        }
        None => wire::DeliveryEvidence::Unknown,
    };
    let mut completion = wire::ProductCompletion::new(
        terminal.duration,
        execution,
        delivery,
        if mcp {
            wire::ProductOutput::Mcp
        } else {
            wire::ProductOutput::Bytes
        },
    );
    completion.timings.work = terminal.facts.request_duration;
    let operation = match terminal.operation {
        native::RemoteOperation::Status => wire::RemoteOperation::Status,
        native::RemoteOperation::Search => wire::RemoteOperation::Search,
        native::RemoteOperation::Event => wire::RemoteOperation::Event,
        native::RemoteOperation::Session => wire::RemoteOperation::Session,
        native::RemoteOperation::Unsupported => wire::RemoteOperation::Unsupported,
    };
    let mut event = wire::RemoteCompletedV1::new(
        operation,
        if mcp {
            wire::RemoteSurface::Mcp
        } else {
            wire::RemoteSurface::Cli
        },
        completion,
    );
    event.result = terminal
        .facts
        .returned
        .map(|count| wire::ProductResultFacts {
            count,
            truncated: None,
        });
    event.read = wire::RemoteReadFacts {
        page_count: (terminal.facts.requests > 0
            && terminal.operation == native::RemoteOperation::Session)
            .then_some(terminal.facts.pages),
        limit: terminal.facts.limit,
        client_limited: terminal
            .facts
            .returned
            .zip(terminal.facts.rendered)
            .map(|(returned, rendered)| rendered < returned),
        complete: terminal.facts.complete,
        exhaustive: terminal.facts.exhaustive,
        has_more: terminal.facts.has_more,
        response_limited: terminal.facts.snippets_truncated.map(|count| count > 0),
        coverage_lag: terminal.facts.coverage_lag,
        query_duration: terminal.facts.request_duration,
    };
    event
}

fn project_failure(
    failure: native::RemoteFailure,
    stage: native::RemoteStage,
) -> wire::ProductFailure {
    use ctx_history_sharing::SharingFailure as F;
    use wire::{ProductFailureClass as C, ProductFailureStage as S};
    let class = match failure {
        native::RemoteFailure::Validation => C::InvalidRequest,
        native::RemoteFailure::Setup => C::Other,
        native::RemoteFailure::Render => C::Other,
        native::RemoteFailure::Output => C::Io,
        native::RemoteFailure::Sharing(failure) => match failure {
            F::Configuration | F::Protocol => C::InvalidRequest,
            F::Credentials | F::Unauthorized => C::Unauthorized,
            F::PolicyDenied | F::Forbidden => C::Forbidden,
            F::NotFound => C::NotFound,
            F::DestinationChanged | F::PolicyConflict | F::Conflict | F::StagingExpired => {
                C::Conflict
            }
            F::Busy | F::TooLarge | F::RateLimited => C::Capacity,
            F::State => C::Store,
            F::NotConnected | F::Unavailable | F::HttpRejected | F::Archive => C::Other,
        },
    };
    wire::ProductFailure {
        class,
        stage: match stage {
            native::RemoteStage::Validation => S::Parse,
            native::RemoteStage::Setup => S::Prepare,
            native::RemoteStage::Request | native::RemoteStage::Complete => S::Execute,
            native::RemoteStage::Render => S::Render,
            native::RemoteStage::Flush => S::Output,
        },
    }
}

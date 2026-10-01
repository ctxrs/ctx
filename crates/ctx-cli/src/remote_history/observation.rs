//! Request-local facts. The composition layer owns consent and delivery.
use ctx_history_server::{CollectionStatus, SearchResponse, SessionPage};
use ctx_history_sharing::SharingFailure;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) type RemoteObserver = Arc<dyn Fn(RemoteCompletion) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteOperation {
    Status,
    Search,
    Event,
    Session,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteStage {
    Validation,
    Setup,
    Request,
    Render,
    Flush,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteFailure {
    Sharing(SharingFailure),
    Validation,
    Setup,
    Render,
    Output,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RemoteReadFacts {
    pub requests: u64,
    pub request_duration: Option<Duration>,
    pub returned: Option<u64>,
    pub rendered: Option<u64>,
    pub pages: u64,
    pub limit: Option<u64>,
    pub continuation_requested: bool,
    pub has_more: Option<bool>,
    pub complete: Option<bool>,
    pub exhaustive: Option<bool>,
    pub snippets_truncated: Option<u64>,
    pub coverage_lag: Option<u64>,
    pub reads_available: Option<bool>,
    /// Available only when the ordinary output path already encodes a buffer.
    pub encoded_bytes: Option<u64>,
    /// CLI writer flush only; never evidence that a human or MCP peer consumed it.
    pub output_flushed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteCompletion {
    pub operation: RemoteOperation,
    pub duration: Duration,
    pub stage: RemoteStage,
    pub failure: Option<RemoteFailure>,
    pub facts: RemoteReadFacts,
}

pub(super) struct Observation {
    operation: RemoteOperation,
    started: Instant,
    pub stage: RemoteStage,
    pub facts: RemoteReadFacts,
}

impl Observation {
    pub fn new(operation: RemoteOperation) -> Self {
        Self {
            operation,
            started: Instant::now(),
            stage: RemoteStage::Validation,
            facts: RemoteReadFacts::default(),
        }
    }

    pub fn request<T>(
        &mut self,
        request: impl FnOnce() -> ctx_history_sharing::Result<T>,
    ) -> anyhow::Result<T> {
        self.stage = RemoteStage::Request;
        let started = Instant::now();
        let result = request();
        self.facts.requests += 1;
        self.facts.request_duration = Some(
            self.facts
                .request_duration
                .unwrap_or_default()
                .saturating_add(started.elapsed()),
        );
        Ok(result?)
    }

    pub fn status(&mut self, status: &CollectionStatus) {
        self.facts.coverage_lag = status
            .stored_sequence
            .checked_sub(status.searchable_sequence);
        self.facts.reads_available = Some(status.reads_available);
    }

    pub fn search(&mut self, response: &SearchResponse, limit: usize) {
        self.status(&response.status);
        self.facts.returned = Some(response.results.len() as u64);
        self.facts.limit = Some(limit as u64);
        self.facts.complete = Some(response.complete);
        self.facts.exhaustive = Some(response.exhaustive);
        self.facts.snippets_truncated = Some(
            response
                .results
                .iter()
                .filter(|hit| hit.snippet_truncated)
                .count() as u64,
        );
    }

    pub fn page(&mut self, page: &SessionPage, continuation: bool) {
        self.facts.returned = Some(
            self.facts
                .returned
                .unwrap_or_default()
                .saturating_add(page.events.len() as u64),
        );
        self.facts.pages += 1;
        self.facts.continuation_requested |= continuation;
        self.facts.has_more = Some(page.next_cursor.is_some());
    }

    pub fn rendered(&mut self, count: u64) {
        self.facts.rendered = Some(
            self.facts
                .rendered
                .unwrap_or_default()
                .saturating_add(count),
        );
    }

    pub fn completion(&self, error: Option<&anyhow::Error>) -> RemoteCompletion {
        let failure = error.map(|error| {
            if let Some(error) = error.downcast_ref::<ctx_history_sharing::Error>() {
                RemoteFailure::Sharing(SharingFailure::from(error))
            } else if error.downcast_ref::<std::io::Error>().is_some()
                && matches!(self.stage, RemoteStage::Render | RemoteStage::Flush)
            {
                RemoteFailure::Output
            } else {
                match self.stage {
                    RemoteStage::Validation => RemoteFailure::Validation,
                    RemoteStage::Setup | RemoteStage::Request => RemoteFailure::Setup,
                    RemoteStage::Render | RemoteStage::Complete => RemoteFailure::Render,
                    RemoteStage::Flush => RemoteFailure::Output,
                }
            }
        });
        RemoteCompletion {
            operation: self.operation,
            duration: self.started.elapsed(),
            stage: self.stage,
            failure,
            facts: self.facts,
        }
    }
}

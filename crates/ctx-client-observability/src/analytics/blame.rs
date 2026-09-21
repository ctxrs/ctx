//! Content-free facts from the Blame result and its final output boundary.

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::{count_bucket, duration_bucket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameTargetKind {
    File,
    Commit,
    PullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameRequestKind {
    FirstRequest,
    Continuation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameResultState {
    Proven,
    Possible,
    Conflicting,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameFreshness {
    Current,
    StaleCommitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameFailureClass {
    InvalidRequest,
    Source,
    Repository,
    Stale,
    Ambiguous,
    Corruption,
    Cancelled,
    Output,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameFailurePhase {
    Setup,
    Query,
    Presentation,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlameFailure {
    pub class: BlameFailureClass,
    pub phase: BlameFailurePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlameResultFacts {
    pub state: BlameResultState,
    /// The validated result's coverage.evaluated, not the attribution count.
    pub evaluated: u64,
    pub freshness: BlameFreshness,
    pub has_more: bool,
}

/// One operation's observed facts. Missing measurements remain absent.
///
/// Adapters fill these from the same result they render, then observe final
/// output. A successful query remains visible when serialization/write fails.
/// No target, cursor, content, identity, or free-form error can enter this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlameTerminalFacts {
    pub target_kind: BlameTargetKind,
    pub request_kind: Option<BlameRequestKind>,
    pub query_duration: Option<Duration>,
    pub result: Option<BlameResultFacts>,
    pub failure: Option<BlameFailure>,
    pub output_served: Option<bool>,
}

impl BlameTerminalFacts {
    pub const fn new(target_kind: BlameTargetKind) -> Self {
        Self {
            target_kind,
            request_kind: None,
            query_duration: None,
            result: None,
            failure: None,
            output_served: None,
        }
    }

    pub(super) fn insert_properties(&self, properties: &mut Map<String, Value>) {
        properties.insert("blame_target_kind".to_owned(), json!(self.target_kind));
        if let Some(kind) = self.request_kind {
            properties.insert("blame_request_kind".to_owned(), json!(kind));
        }
        if let Some(duration) = self.query_duration {
            properties.insert(
                "blame_query_duration_bucket".to_owned(),
                json!(duration_bucket(duration).as_str()),
            );
        }
        if let Some(result) = self.result {
            properties.insert("blame_result_state".to_owned(), json!(result.state));
            properties.insert(
                "blame_result_count_bucket".to_owned(),
                json!(count_bucket(result.evaluated).as_str()),
            );
            properties.insert("blame_freshness".to_owned(), json!(result.freshness));
            properties.insert("blame_has_more".to_owned(), json!(result.has_more));
        }
        if let Some(failure) = self.failure {
            properties.insert("blame_failure_class".to_owned(), json!(failure.class));
            properties.insert("blame_failure_phase".to_owned(), json!(failure.phase));
        }
        if let Some(served) = self.output_served {
            properties.insert("blame_output_served".to_owned(), json!(served));
        }
    }
}

#[cfg(test)]
mod tests;

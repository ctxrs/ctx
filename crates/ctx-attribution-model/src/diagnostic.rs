//! Pure diagnostic data. Execution owns error mapping and recovery decisions.
pub use crate::{BlameDiagnosticCandidate, BlameDiagnosticReason};
use serde::Serialize;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameDiagnostic {
    pub error: &'static str,
    pub error_code: &'static str,
    pub reason: BlameDiagnosticReason,
    pub message: &'static str,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<BlameDiagnosticFreshness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<BlameNextAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<BlameDiagnosticCandidate>,
    #[serde(skip_serializing_if = "is_false")]
    pub candidates_truncated: bool,
}

impl fmt::Display for BlameDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.error_code)
    }
}

impl Error for BlameDiagnostic {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameDiagnosticFreshness {
    pub state: BlameFreshnessState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameFreshnessState {
    StaleCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameNextAction {
    pub kind: BlameNextActionKind,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameNextActionKind {
    CheckStatus,
    SearchCore,
    ImportAll,
}

fn is_false(value: &bool) -> bool {
    !*value
}

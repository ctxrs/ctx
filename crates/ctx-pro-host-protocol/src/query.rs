use serde::{Deserialize, Serialize};

use super::{
    ErrorClass, JournalCheckpoint, ProtocolError, QueryKind, ResourceSelector,
    MAX_QUERY_CURSOR_BYTES, MAX_QUERY_RESULTS,
};

/// Exact journal checkpoint that a cited query requires the derived graph to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySnapshotExpectation {
    pub checkpoint: JournalCheckpoint,
    pub projection_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub kind: QueryKind,
    pub target: ResourceSelector,
    pub limit: u32,
    pub cursor: Option<String>,
    pub expected_snapshot: QuerySnapshotExpectation,
}

impl QueryRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.expected_snapshot.checkpoint.validate()?;
        if self.limit == 0 || self.limit > MAX_QUERY_RESULTS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                format!("query limit must be between 1 and {MAX_QUERY_RESULTS}"),
            ));
        }
        if self.cursor.as_deref().is_some_and(|cursor| {
            cursor.is_empty() || cursor.len() > MAX_QUERY_CURSOR_BYTES || !cursor.is_ascii()
        }) {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                format!("query cursor must contain 1 to {MAX_QUERY_CURSOR_BYTES} ASCII bytes"),
            ));
        }
        self.target.validate()
    }
}

use crate::{CoreMaterializationReceipt, ErrorClass, ProtocolError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreProjectionCurrentness {
    NotMaterialized,
    Partial,
    Stale,
    NeedsRebuild,
    Current,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializedCoverage {
    NotMaterialized,
    Partial,
    Complete,
    Empty,
    Abstained,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCoverage {
    pub repository_candidate_events: u64,
    pub logical_binding_events: u64,
    pub certified_live_root_access_events: u64,
    pub file_evidence_events: u64,
    pub exact_commit_evidence_events: u64,
    pub exact_pull_request_evidence_events: u64,
}

impl RepositoryCoverage {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate_for_receipt(
        &self,
        receipt: Option<&CoreMaterializationReceipt>,
    ) -> Result<(), ProtocolError> {
        let Some(receipt) = receipt else {
            if self.is_empty() {
                return Ok(());
            }
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "repository coverage requires a completed Core receipt",
            ));
        };
        for (label, count) in [
            (
                "repository candidate events",
                self.repository_candidate_events,
            ),
            ("logical binding events", self.logical_binding_events),
            (
                "certified live-root access events",
                self.certified_live_root_access_events,
            ),
            ("file evidence events", self.file_evidence_events),
            (
                "exact commit evidence events",
                self.exact_commit_evidence_events,
            ),
            (
                "exact pull-request evidence events",
                self.exact_pull_request_evidence_events,
            ),
        ] {
            if count > receipt.event_count {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    format!("repository coverage {label} exceeds Core receipt event count"),
                ));
            }
        }
        if self.logical_binding_events > self.repository_candidate_events {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "repository coverage logical binding events exceed candidate coverage",
            ));
        }
        for (label, count) in [
            (
                "certified live-root access events",
                self.certified_live_root_access_events,
            ),
            ("file evidence events", self.file_evidence_events),
            (
                "exact commit evidence events",
                self.exact_commit_evidence_events,
            ),
            (
                "exact pull-request evidence events",
                self.exact_pull_request_evidence_events,
            ),
        ] {
            if count > self.logical_binding_events {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    format!("repository coverage {label} exceeds logical binding coverage"),
                ));
            }
        }
        Ok(())
    }
}

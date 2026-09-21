use crate::BlameResult;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameResultFreshness {
    Current,
    StaleCommitted,
}

#[derive(Debug, Clone)]
pub struct HostedBlameResult {
    pub result: BlameResult,
    pub freshness: BlameResultFreshness,
}

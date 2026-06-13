use super::*;

pub struct SessionTurnToolCountDeltas {
    pub total: i64,
    pub pending: i64,
    pub running: i64,
    pub completed: i64,
    pub failed: i64,
}

include!("records.rs");
include!("history.rs");
include!("tools.rs");

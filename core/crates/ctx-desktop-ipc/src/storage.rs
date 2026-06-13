use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesktopStorageBatchOp {
    Delete {
        key: String,
    },
    Set {
        key: String,
        #[ts(type = "unknown")]
        value: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopUiStateResetReason {
    SchemaMismatch,
    InvalidUiStateDb,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesktopStorageNotice {
    UiStateReset { reason: DesktopUiStateResetReason },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopStorageGetReq {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopStorageBatchReq {
    pub ops: Vec<DesktopStorageBatchOp>,
}

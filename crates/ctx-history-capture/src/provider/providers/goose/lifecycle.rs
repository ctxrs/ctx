use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GooseNativeInventorySummary {
    pub(super) native_session_rows: u64,
    pub(super) native_message_rows: u64,
    pub(super) session_identity_digest: String,
    pub(super) session_identity_samples: Vec<String>,
}

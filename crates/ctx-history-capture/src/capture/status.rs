#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct NanoClawMessageRow {
    pub(crate) source: &'static str,
    pub(crate) id: String,
    pub(crate) seq: Option<i64>,
    pub(crate) kind: Option<String>,
    pub(crate) timestamp: Option<i64>,
    pub(crate) status: Option<String>,
    pub(crate) in_reply_to: Option<String>,
    pub(crate) platform_id: Option<String>,
    pub(crate) channel_type: Option<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) trigger: Option<String>,
    pub(crate) source_session_id: Option<String>,
    pub(crate) on_wake: Option<i64>,
}

pub(crate) fn provider_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => RunStatus::Partial,
    }
}

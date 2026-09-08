use super::*;

/// Schema-v1 source-refresh IPC encoder.
pub(super) struct SourceBackedRefreshRequest<'a> {
    mode: SourceBackedRefreshMode,
    request: &'a RefreshRequest,
}

impl<'a> SourceBackedRefreshRequest<'a> {
    pub(super) fn new(mode: SourceBackedRefreshMode, request: &'a RefreshRequest) -> Self {
        Self { mode, request }
    }

    pub(super) fn to_json(&self) -> Result<Value> {
        Ok(compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": self.request.request_id(),
            "mode": self.mode.as_str(),
            "trigger": self.request.trigger().as_str(),
            "refresh_intent": self.request.intent().to_json(),
        })))
    }
}

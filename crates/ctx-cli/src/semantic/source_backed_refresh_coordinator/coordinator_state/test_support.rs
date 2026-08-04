use super::*;

impl CoreRefreshEngine {
    pub(in crate::semantic) fn status_for_test(&self, request_id: &str) -> Option<Value> {
        self.status(request_id)
    }

    pub(in crate::semantic) fn logical_continuation_is_fully_covered_for_test(
        &self,
        request_id: &str,
    ) -> bool {
        self.lock_state()
            .manual_all_continuations
            .get(request_id)
            .is_some_and(ManualAllContinuation::is_fully_covered)
    }

    pub(in crate::semantic) fn handle_ipc_request_with_admission_fence_for_test(
        &self,
        data_root: &Path,
        request: &Value,
        observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<Option<Value>> {
        let response = self.handle_ipc_request(data_root, request)?;
        let Some(request_id) = response
            .as_ref()
            .and_then(|response| response.get("request_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(response);
        };
        if self
            .status(&request_id)
            .is_some_and(|status| status["request_state"] == "admission_pending")
        {
            self.complete_pending_admission_for_test(data_root, &request_id, observations)?;
            return Ok(self.status(&request_id));
        }
        Ok(response)
    }
}

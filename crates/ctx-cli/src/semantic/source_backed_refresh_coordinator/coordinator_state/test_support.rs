use super::*;

impl CoreRefreshEngine {
    pub(in crate::semantic) fn handle_ipc_request_with_admission_fence_for_test(
        &self,
        data_root: &Path,
        request: &Value,
        observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<Option<Value>> {
        self.handle_ipc_request_with_admission_fence(data_root, request, move |_, _| {
            Ok(observations.clone())
        })
    }
}

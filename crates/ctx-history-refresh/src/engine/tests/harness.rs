use super::*;

pub(super) const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
pub(super) const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

pub(super) struct CoreRefreshEngine(pub(super) super::super::CoreRefreshEngine);

impl std::ops::Deref for CoreRefreshEngine {
    type Target = super::super::CoreRefreshEngine;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl CoreRefreshEngine {
    pub(super) fn new() -> Self {
        Self(test_refresh_engine())
    }

    pub(super) fn with_executor(executor: Arc<dyn SourceBackedRefreshExecutor>) -> Self {
        Self(test_refresh_engine_with_executor(executor))
    }

    pub(super) fn with_status_writer_for_test(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        writer: TestStatusWriter,
    ) -> Self {
        Self(test_refresh_engine_with_status_writer(executor, writer))
    }

    pub(super) fn status(&self, request_id: &str) -> Option<Value> {
        self.0.status_for_test(request_id)
    }

    pub(super) fn handle_ipc_request(
        &self,
        data_root: &Path,
        request: &Value,
    ) -> Result<Option<Value>> {
        match request.get("op").and_then(Value::as_str) {
            Some(SOURCE_REFRESH_REQUEST_OP) => {
                let admission = self
                    .0
                    .submit(data_root, test_refresh_submission_from_json(request)?)?;
                let (status, barrier) = admission.into_parts();
                if let Some(barrier) = barrier {
                    barrier.release(&self.0);
                }
                Ok(Some(status.schema_v1_fields().clone()))
            }
            Some(SOURCE_REFRESH_STATUS_OP) => {
                let request_id = request
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|request_id| !request_id.is_empty())
                    .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
                Ok(self.status(request_id))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn handle_ipc_request_with_admission_fence_for_test(
        &self,
        data_root: &Path,
        request: &Value,
        observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<Option<Value>> {
        let admission = self
            .0
            .submit(data_root, test_refresh_submission_from_json(request)?)?;
        let (status, barrier) = admission.into_parts();
        let request_id = status.request_id().map(str::to_owned);
        if let Some(barrier) = barrier {
            barrier.release(&self.0);
        }
        if status.schema_v1_fields()["request_state"] == "admission_pending" {
            self.0.complete_pending_admission_for_test(
                data_root,
                request_id.as_deref().expect("pending request ID"),
                observations,
            )?;
            return Ok(request_id
                .as_deref()
                .and_then(|request_id| self.status(request_id)));
        }
        Ok(Some(status.schema_v1_fields().clone()))
    }
}

fn test_refresh_submission_from_json(request: &Value) -> Result<RefreshSubmission> {
    let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
    if !matches!(mode, "background" | "wait") {
        bail!("invalid daemon source refresh mode `{mode}`");
    }
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("daemon source refresh request operation is missing"))
        .and_then(|operation| match operation {
            "refresh" => Ok(RefreshOperation::Refresh),
            "import" => Ok(RefreshOperation::Import),
            operation => bail!("invalid daemon source refresh operation `{operation}`"),
        })?;
    let explicit_catalog = request.get("explicit_source_catalog");
    match (operation, mode, explicit_catalog) {
        (RefreshOperation::Refresh, _, Some(_)) => {
            bail!("refresh operation cannot carry explicit source catalog authority")
        }
        (RefreshOperation::Import, "background", _) => {
            bail!("import operation requires daemon refresh mode `wait`")
        }
        (RefreshOperation::Import, _, None) => {
            bail!("import operation requires explicit source catalog authority")
        }
        _ => {}
    }
    let request_id = match request.get("request_id") {
        Some(Value::String(request_id)) if !request_id.is_empty() => {
            Uuid::parse_str(request_id)
                .context("daemon source refresh logical request ID must be a UUID")?;
            request_id.clone()
        }
        None => Uuid::now_v7().to_string(),
        Some(_) => bail!("daemon source refresh logical request ID is invalid"),
    };
    let fresh_after_admitted_snapshot = match request.get("fresh_after_admitted_snapshot") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => {
            bail!("daemon source refresh fresh-after-admitted-snapshot requirement must be boolean")
        }
    };
    if operation == RefreshOperation::Refresh
        && mode == "background"
        && fresh_after_admitted_snapshot
    {
        bail!("background source refresh cannot require a fresh admission snapshot");
    }
    let requested_catalog = explicit_catalog
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
    let refresh_scope = request
        .get("refresh_scope")
        .filter(|value| !value.is_null())
        .map(|value| refresh_scope_from_json(Some(value)))
        .transpose()?
        .unwrap_or(SourceBackedRefreshScope::All);
    Ok(RefreshSubmission::new(
        request_id,
        operation,
        requested_catalog,
        refresh_scope,
        fresh_after_admitted_snapshot,
        operation == RefreshOperation::Refresh && mode == "background",
    ))
}

pub(super) fn load_explicit_source_catalog_authority(
    _data_root: &Path,
) -> Result<ExplicitSourceCatalogAuthority> {
    Ok(crate::explicit_source_catalog_authority_for_test(0))
}

pub(super) fn pin_published_generation(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    pin_test_published_generation(data_root)
}

pub(super) fn pin_active_verified_generation(
    data_root: &Path,
) -> Result<PinnedSourceBackedGeneration> {
    pin_test_active_verified_generation(data_root)
}

pub(super) fn open_published_generation(data_root: &Path) -> Result<Option<VerifiedIndex>> {
    open_test_published_generation(data_root)
}

use super::*;

pub(super) const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";

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

    pub(super) fn with_executor_and_admitted_routes(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
    ) -> Self {
        Self(test_refresh_engine_with_executor_and_admitted_routes(
            executor, routes,
        ))
    }

    pub(super) fn with_journal_executor_and_admitted_routes(
        journal: Arc<dyn RefreshJournal>,
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
    ) -> Self {
        Self(
            test_refresh_engine_with_journal_executor_and_admitted_routes(
                journal, executor, routes,
            ),
        )
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
                if request.get("mode").and_then(Value::as_str) == Some("background") {
                    let request_id = request
                        .get("request_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| Uuid::now_v7().to_string());
                    return self
                        .0
                        .maintenance_wake(data_root, request_id)
                        .map(|status| Some(status.schema_v1_fields().clone()));
                }
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

fn test_refresh_submission_from_json(request: &Value) -> Result<RefreshRequest> {
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
    let all_selection = request
        .get("refresh_selector")
        .and_then(|selector| selector.get("kind"))
        .and_then(Value::as_str)
        == Some("all_automatic");
    match (operation, mode, explicit_catalog) {
        (RefreshOperation::Refresh, _, Some(_)) => {
            bail!("refresh operation cannot carry explicit source catalog authority")
        }
        (RefreshOperation::Import, "background", _) => {
            bail!("import operation requires daemon refresh mode `wait`")
        }
        (RefreshOperation::Import, _, None) if !all_selection => {
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
    let intent = match (operation, requested_catalog, fresh_after_admitted_snapshot) {
        (RefreshOperation::Import, Some(authority), _) => {
            RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority))
        }
        (RefreshOperation::Import, None, _) => RefreshIntent::SelectedImport(RefreshSelection::All),
        (RefreshOperation::Refresh, None, true) => {
            RefreshIntent::SelectedImport(RefreshSelection::All)
        }
        (RefreshOperation::Refresh, None, false) => RefreshIntent::AutomaticMaintenance,
        (RefreshOperation::Refresh, Some(_), _) => {
            unreachable!("validated request authority")
        }
    };
    let trigger = request
        .get("trigger")
        .and_then(Value::as_str)
        .map(str::parse)
        .transpose()?
        .unwrap_or(match intent {
            RefreshIntent::AutomaticMaintenance => RefreshRequestTrigger::Search,
            RefreshIntent::SelectedImport(_) => RefreshRequestTrigger::Import,
        });
    Ok(RefreshRequest::new(request_id, intent, trigger))
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

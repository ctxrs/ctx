use ctx_core::ids::TaskId;

use super::errors::{api_error, internal_api_error, ApiResult, SubagentErrorKind};

pub(super) fn default_catalog_model_id(
    catalog: Option<&ctx_session_tools::model_resolution::ModelCatalog>,
) -> Option<&str> {
    catalog.and_then(ctx_session_tools::model_resolution::ModelCatalog::default_model_id)
}

pub(super) async fn ensure_requested_labels_available(
    store: &ctx_store::Store,
    task_id: TaskId,
    labels: &[String],
) -> ApiResult<()> {
    for label in labels {
        if store
            .subagent_label_exists(task_id, label)
            .await
            .map_err(internal_api_error)?
        {
            return Err(api_error(
                SubagentErrorKind::BadRequest,
                format!("subagent label '{label}' already exists for this task"),
            ));
        }
    }

    Ok(())
}

use ctx_session_tools::model_resolution::resolve_model_id;

use super::{SubagentChildInit, SubagentChildInitItem};
use crate::daemon::sessions::subagents::errors::{api_error, ApiResult, SubagentErrorKind};
use crate::daemon::sessions::subagents::request::default_catalog_model_id;

pub(super) struct ResolvedChildModel {
    pub(super) provider_id: String,
    pub(super) model_id: String,
    pub(super) full_model_id: String,
    pub(super) reasoning_effort: Option<String>,
}

pub(super) async fn resolve_child_model(
    init: &SubagentChildInit,
    item: &SubagentChildInitItem,
) -> ApiResult<ResolvedChildModel> {
    let provider_id = resolve_provider_id(init, item).await;
    let catalog = init
        .model_catalogs
        .get(&provider_id)
        .and_then(|value| value.as_ref());
    let fallback_model = fallback_model_id(init, item, &provider_id, catalog).await?;
    let resolved = resolve_model_id(
        item.agent.model.as_deref(),
        item.agent.reasoning_effort.as_deref(),
        fallback_model.as_deref(),
        catalog,
    )
    .map_err(|error| api_error(SubagentErrorKind::BadRequest, error))?;

    Ok(ResolvedChildModel {
        provider_id,
        model_id: resolved.model_id,
        full_model_id: resolved.full_model_id,
        reasoning_effort: resolved.reasoning_effort,
    })
}

async fn resolve_provider_id(init: &SubagentChildInit, item: &SubagentChildInitItem) -> String {
    let harness_defaulted = item.agent.harness.is_none();
    let provider_id = item
        .agent
        .harness
        .as_deref()
        .unwrap_or(&init.parent.provider_id)
        .trim()
        .to_string();
    if harness_defaulted {
        init.host
            .emit_product_fallback_applied_counter(
                "sessions.subagent_init",
                "harness_default_parent",
                None,
            )
            .await;
    }
    provider_id
}

async fn fallback_model_id(
    init: &SubagentChildInit,
    item: &SubagentChildInitItem,
    provider_id: &str,
    catalog: Option<&ctx_session_tools::model_resolution::ModelCatalog>,
) -> ApiResult<Option<String>> {
    let fallback_model = if item.agent.model.is_none() {
        if provider_id == init.parent.provider_id {
            Some(init.parent.model_id.clone())
        } else {
            default_catalog_model_id(catalog).map(ToOwned::to_owned)
        }
    } else {
        None
    };
    if item.agent.model.is_none() && fallback_model.is_none() {
        init.host
            .emit_compat_payload_reject_counter(
                "sessions.subagent_init",
                "missing_model_without_default",
                Some(("provider_id", provider_id)),
            )
            .await;
        return Err(api_error(
            SubagentErrorKind::BadRequest,
            format!("model is required for harness '{provider_id}'"),
        ));
    }
    if item.agent.model.is_none() {
        let fallback = if provider_id == init.parent.provider_id {
            "model_default_parent"
        } else {
            "model_default_catalog"
        };
        init.host
            .emit_product_fallback_applied_counter("sessions.subagent_init", fallback, None)
            .await;
    }
    Ok(fallback_model)
}

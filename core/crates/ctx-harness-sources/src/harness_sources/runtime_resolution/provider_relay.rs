use std::collections::HashMap;

use anyhow::Result;

use super::provider_fs::endpoint_preferred_model_id;
use super::*;

pub(super) fn ctx_managed_relay_env(
    canonical: &'static str,
    endpoint: &HarnessEndpointRecordInternal,
) -> Result<HashMap<String, String>> {
    validation::ensure_shape_compatible(canonical, endpoint.api_shape)?;
    if endpoint.api_shape != HarnessApiShape::OpenaiResponses {
        anyhow::bail!(
            "ctx-managed relay endpoint '{}' for {} must use openai_responses shape",
            endpoint.name,
            canonical
        );
    }
    let base_url = validation::endpoint_base_url_or_err(endpoint)?;
    let model_id = endpoint_preferred_model_id(endpoint).ok_or_else(|| {
        anyhow::anyhow!(
            "ctx-managed relay endpoint '{}' for {} is missing a concrete model id",
            endpoint.name,
            canonical
        )
    })?;
    let mut env = HashMap::new();
    env.insert(
        CTX_PROVIDER_ROUTE_BACKEND_ENV.to_string(),
        HarnessRouteBackend::CtxManagedRelay.as_str().to_string(),
    );
    env.insert(CTX_LLM_RELAY_BASE_URL_ENV.to_string(), base_url);
    env.insert(CTX_LLM_RELAY_MODEL_ENV.to_string(), model_id);
    Ok(env)
}

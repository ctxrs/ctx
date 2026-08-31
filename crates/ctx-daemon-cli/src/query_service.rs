use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
#[cfg(test)]
use ctx_semantic_model::semantic_model_contract;
use ctx_semantic_model::SemanticModelContract;
use serde_json::{json, Value};

use crate::compact_json;

pub(crate) use ctx_daemon_service::{daemon_query_request, DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION};

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    let Ok(contract) = selected_semantic_contract(data_root) else {
        return false;
    };
    daemon_query_service_ping(data_root, &contract).is_ok_and(|response| {
        response.is_some_and(|value| ping_response_is_compatible(&value, &contract))
    })
}

fn daemon_query_service_ping(
    data_root: &Path,
    contract: &SemanticModelContract,
) -> Result<Option<Value>> {
    daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
            "op": "ping",
            "executor_route_identity": contract.executor_route_identity(),
        })),
        Duration::from_secs(1),
        1024,
    )
}

fn ping_response_is_compatible(response: &Value, contract: &SemanticModelContract) -> bool {
    let route_identity = contract.executor_route_identity();
    response.get("ok").and_then(Value::as_bool) == Some(true)
        && response.get("schema_version").and_then(Value::as_u64)
            == Some(DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION)
        && response.get("model_key").and_then(Value::as_str) == Some(contract.model_key())
        && response
            .get("model_contract_fingerprint")
            .and_then(Value::as_str)
            == Some(contract.fingerprint())
        && response
            .get("executor_route_identity")
            .and_then(Value::as_str)
            == Some(route_identity.as_str())
}

pub(crate) fn daemon_query_service_embedding_runtime(
    data_root: &Path,
    contract: &SemanticModelContract,
) -> Option<Value> {
    daemon_query_service_ping(data_root, contract)
        .ok()
        .flatten()
        .filter(|response| ping_response_is_compatible(response, contract))
        .and_then(|response| {
            response
                .get("embedding_runtime")
                .filter(|value| value.is_object())
                .cloned()
        })
}

fn selected_semantic_contract(data_root: &Path) -> Result<SemanticModelContract> {
    Ok(crate::composition::load_runtime_config(data_root)?
        .semantic_model_contract()
        .clone())
}

pub fn wait_for_daemon_query_service(data_root: &Path, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        if daemon_query_service_available(data_root) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatible_ping_response() -> Value {
        let contract = semantic_model_contract();
        compact_json(json!({
            "ok": true,
            "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
            "model_key": contract.model_key(),
            "model_contract_fingerprint": contract.fingerprint(),
            "executor_route_identity": contract.executor_route_identity(),
            "embedding_runtime": null,
            "busy": false,
        }))
    }

    #[test]
    fn readiness_accepts_the_current_ping_contract() {
        assert!(ping_response_is_compatible(
            &compatible_ping_response(),
            semantic_model_contract()
        ));
    }

    #[test]
    fn readiness_rejects_the_frozen_v1_ping_without_a_routing_fence() {
        let mut response = compatible_ping_response();
        let response_object = response.as_object_mut().expect("object");
        response_object.insert("schema_version".to_owned(), json!(1));
        response_object.remove("executor_route_identity");

        assert!(!ping_response_is_compatible(
            &response,
            semantic_model_contract()
        ));
    }

    #[test]
    fn readiness_rejects_missing_or_mismatched_ping_contract_fields() {
        let valid = compatible_ping_response();
        let mut invalid = Vec::new();

        let mut response = valid.clone();
        response.as_object_mut().expect("object").remove("ok");
        invalid.push(("missing ok", response));

        let mut response = valid.clone();
        response["ok"] = Value::Bool(false);
        invalid.push(("negative ok", response));

        for (case, field, mismatch) in [
            ("schema", "schema_version", json!(1)),
            ("model key", "model_key", json!("different-model")),
            (
                "model contract fingerprint",
                "model_contract_fingerprint",
                json!("sha256:different"),
            ),
            (
                "executor route identity",
                "executor_route_identity",
                json!("sha256:different"),
            ),
        ] {
            let mut missing = valid.clone();
            missing.as_object_mut().expect("object").remove(field);
            invalid.push((case, missing));

            let mut mismatched = valid.clone();
            mismatched[field] = mismatch;
            invalid.push((case, mismatched));
        }

        for (case, response) in invalid {
            assert!(
                !ping_response_is_compatible(&response, semantic_model_contract()),
                "readiness accepted {case}: {response}"
            );
        }
    }

    #[test]
    fn readiness_compares_the_selected_external_contract() {
        let external = crate::SemanticEmbeddingExecutorConfig::http(
            "https://embed.example.test/base",
            crate::ExternalSemanticSpace::new("acme/multilingual-v2", 768).unwrap(),
        )
        .unwrap();
        let contract = external.contract();
        let response = compact_json(json!({
            "ok": true,
            "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
            "model_key": contract.model_key(),
            "model_contract_fingerprint": contract.fingerprint(),
            "executor_route_identity": contract.executor_route_identity(),
            "embedding_runtime": null,
            "busy": false,
        }));

        assert!(ping_response_is_compatible(&response, contract));
        assert!(!ping_response_is_compatible(
            &response,
            semantic_model_contract()
        ));
    }
}

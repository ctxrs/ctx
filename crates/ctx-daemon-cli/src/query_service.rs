use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use ctx_semantic_model::semantic_model_contract;
use serde_json::{json, Value};

use crate::compact_json;

pub(crate) use ctx_daemon_service::daemon_query_request;

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    daemon_query_service_ping(data_root)
        .is_ok_and(|response| response.is_some_and(|value| ping_response_is_compatible(&value)))
}

fn daemon_query_service_ping(data_root: &Path) -> Result<Option<Value>> {
    daemon_query_request(
        data_root,
        compact_json(json!({"schema_version": 1, "op": "ping"})),
        Duration::from_secs(1),
        1024,
    )
}

fn ping_response_is_compatible(response: &Value) -> bool {
    let contract = semantic_model_contract();
    let fingerprint = contract.fingerprint();
    response.get("ok").and_then(Value::as_bool) == Some(true)
        && response.get("schema_version").and_then(Value::as_u64) == Some(1)
        && response.get("model_key").and_then(Value::as_str) == Some(contract.model_key())
        && match response
            .get("model_contract_fingerprint")
            .and_then(Value::as_str)
        {
            Some(response_fingerprint) => response_fingerprint == fingerprint,
            None => contract.supports_frozen_legacy_v1(),
        }
}

pub(crate) fn daemon_query_service_embedding_runtime(data_root: &Path) -> Option<Value> {
    daemon_query_service_ping(data_root)
        .ok()
        .flatten()
        .filter(ping_response_is_compatible)
        .and_then(|response| {
            response
                .get("embedding_runtime")
                .filter(|value| value.is_object())
                .cloned()
        })
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
            "schema_version": 1,
            "model_key": contract.model_key(),
            "model_contract_fingerprint": contract.fingerprint(),
            "embedding_runtime": null,
            "busy": false,
        }))
    }

    #[test]
    fn readiness_accepts_the_current_ping_contract() {
        assert!(ping_response_is_compatible(&compatible_ping_response()));
    }

    #[test]
    fn readiness_accepts_the_frozen_legacy_v1_ping_without_a_fingerprint() {
        let mut response = compatible_ping_response();
        response
            .as_object_mut()
            .expect("object")
            .remove("model_contract_fingerprint");

        assert!(ping_response_is_compatible(&response));
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
            ("schema", "schema_version", json!(2)),
            ("model key", "model_key", json!("different-model")),
            (
                "model contract fingerprint",
                "model_contract_fingerprint",
                json!("sha256:different"),
            ),
        ] {
            if field != "model_contract_fingerprint" {
                let mut missing = valid.clone();
                missing.as_object_mut().expect("object").remove(field);
                invalid.push((case, missing));
            }

            let mut mismatched = valid.clone();
            mismatched[field] = mismatch;
            invalid.push((case, mismatched));
        }

        for (case, response) in invalid {
            assert!(
                !ping_response_is_compatible(&response),
                "readiness accepted {case}: {response}"
            );
        }
    }
}

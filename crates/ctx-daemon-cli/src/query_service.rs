use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use serde_json::{json, Value};

use crate::compact_json;

pub(crate) use ctx_daemon_service::daemon_query_request;

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    daemon_query_service_ping(data_root)
        .is_ok_and(|response| response.is_some_and(|value| response_ok(&value)))
}

fn daemon_query_service_ping(data_root: &Path) -> Result<Option<Value>> {
    daemon_query_request(
        data_root,
        compact_json(json!({"schema_version": 1, "op": "ping"})),
        Duration::from_secs(1),
        1024,
    )
}

fn response_ok(response: &Value) -> bool {
    response.get("ok").and_then(Value::as_bool) == Some(true)
}

pub(crate) fn daemon_query_service_embedding_runtime(data_root: &Path) -> Option<Value> {
    daemon_query_service_ping(data_root)
        .ok()
        .flatten()
        .filter(response_ok)
        .and_then(|response| {
            response
                .get("embedding_runtime")
                .filter(|value| value.is_object())
                .cloned()
        })
}

pub fn wait_for_daemon_query_service(data_root: &Path, timeout: Duration) -> bool {
    if !ctx_semantic_model::semantic_query_service_supported() {
        return false;
    }
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

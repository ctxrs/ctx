use std::path::Path;

use serde_json::{json, Value};

use super::{daemon_semantic_job_path, read_daemon_job_status, DaemonSemanticGeneration};

pub(super) fn bind_semantic_generation(
    data_root: &Path,
    mut job: Value,
    generation: DaemonSemanticGeneration<'_>,
) -> Value {
    let semantic_contract = generation.contract;
    job["model_key"] = Value::String(semantic_contract.model_key().to_owned());
    job["model_contract_fingerprint"] = Value::String(semantic_contract.fingerprint().to_owned());
    if let Ok(fingerprint) =
        ctx_semantic_index::source_backed_semantic_contract_fingerprint(semantic_contract)
    {
        job["source_contract_fingerprint"] = Value::String(fingerprint);
    }
    job["core_generation_id"] =
        Value::String(generation.source_generation.generation_id().to_owned());
    // Retry, resource, and deadline receipts have no semantic work of their
    // own. Carry the same-target durable sequence forward so status churn can
    // neither erase nor regress the CLI's sole progress authority.
    if let Some(previous) = read_daemon_job_status(&daemon_semantic_job_path(data_root))
        .filter(|previous| target_matches(previous, &job))
        .and_then(|previous| sequence(&previous))
    {
        let current = sequence(&job).unwrap_or_default();
        if previous > current {
            job["semantic_progress_sequence"] = json!(previous);
        }
    }
    job
}

fn sequence(job: &Value) -> Option<u64> {
    job.get("semantic_progress_sequence")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
}

fn target_matches(left: &Value, right: &Value) -> bool {
    [
        "core_generation_id",
        "model_contract_fingerprint",
        "source_contract_fingerprint",
    ]
    .into_iter()
    .all(|field| {
        left.get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| right.get(field).and_then(Value::as_str) == Some(value))
    })
}

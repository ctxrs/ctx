use chrono::{DateTime, Utc};

mod dialect;
mod normalization;

pub(crate) mod cline_nativepath;

pub(crate) use dialect::task_json_provider;
pub(crate) use normalization::task_json_result_content;
pub(crate) use normalization::{
    task_json_event_text, task_json_event_type, task_json_string_field, task_json_time_field,
    TaskJsonEventInput,
};

// Reconstructs only the provider-supplied hash authority needed to hydrate
// complete-content locators emitted by released task-JSON imports.
pub(crate) fn task_json_event_hash(
    spec: dialect::TaskJsonProviderSpec,
    task_id: &str,
    input: TaskJsonEventInput,
    event_ordinal: usize,
    occurred_at: DateTime<Utc>,
) -> String {
    let event =
        normalization::task_json_event_draft(spec, task_id, input, event_ordinal, occurred_at);
    // Keep the released normalization path authoritative for every canonical
    // field even though hydration needs only its provider-supplied hash.
    let _canonical_fields = (
        &event.provider_event_index,
        &event.cursor,
        &event.event_type,
        &event.role,
        &event.occurred_at,
        &event.fidelity,
        &event.idempotency_key,
        &event.payload,
        &event.metadata,
    );
    event.provider_event_hash
}

#[cfg(test)]
pub(crate) const TASK_JSON_RESULT_CONTENT_PROFILE: &str = "task-json.result-body.v1";

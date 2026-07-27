use crate::Result;

mod native_path;
mod schema;
mod source;

#[cfg(test)]
mod tests;

pub(crate) use self::schema::mistral_vibe_result_content;
pub(crate) use native_path::import_mistral_vibe_nativepath;

pub(super) const MISTRAL_VIBE_CAPTURE_REVISION: u32 = 4;
pub(super) const MISTRAL_VIBE_POLICY_REVISION: u32 = 8;
const MISTRAL_VIBE_MAX_ID_BYTES: usize = 4 * 1024;
pub(crate) const MISTRAL_VIBE_RESULT_CONTENT_PROFILE: &str = "mistral-vibe.result-body.v1";

pub(crate) fn mistral_vibe_complete_content_record(
    value: &serde_json::Value,
    line_number: usize,
) -> Option<(String, String)> {
    let role = value
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let event_type = schema::mistral_vibe_event_type(role, value);
    (event_type == ctx_history_core::EventType::Message).then(|| {
        (
            schema::mistral_vibe_event_text(role, value, event_type),
            schema::mistral_vibe_event_id(value, line_number, role),
        )
    })
}

pub(crate) fn mistral_vibe_complete_content_source_from_admitted(
    metadata: &std::fs::Metadata,
    messages: &std::fs::Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    Ok((
        source::mistral_vibe_complete_content_revision_from_admitted(metadata, messages)?,
        path_identity,
    ))
}

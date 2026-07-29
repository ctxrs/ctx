mod content_policy;
mod io;
mod result_content;
mod result_evidence;
mod value;

pub(crate) use content_policy::{
    compact_provider_result_payload, provider_policy_body, provider_policy_event_text,
};
pub(crate) use io::provider_optional_regular_file;
pub(crate) use result_content::provider_normalized_result_value;
pub(crate) use result_evidence::{
    provider_output_event_is_failure, provider_result_identifier_evidence,
    provider_result_outcome_evidence,
};
#[allow(unused_imports)]
pub(crate) use value::{
    capped_text, provider_block_event_type, provider_block_text, provider_capped_json,
    provider_capped_json_value, provider_explicit_result_value_text, provider_json_text,
    provider_line_from_index, provider_local_preview, provider_message_has_part_kind,
    provider_message_id, provider_message_parts, provider_nonnegative_i64_to_u64,
    provider_part_text, provider_required_timestamp_millis, provider_required_timestamp_seconds,
    provider_role, provider_role_from_message, provider_string_field,
    provider_timestamp_from_fields, provider_timestamp_millis, provider_timestamp_seconds,
    provider_timestamp_seconds_to_datetime, provider_timestamp_value, provider_value_text,
    text_id_index,
};

#[cfg(test)]
use result_evidence::{MAX_RESULT_EVIDENCE_CALL_ID_CHARS, MAX_RESULT_EVIDENCE_IDENTIFIERS};

#[cfg(test)]
mod tests;

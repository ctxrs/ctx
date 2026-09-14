use std::path::Path;

use ctx_history_core::CaptureProvider;
use ctx_history_native_jsonl_parsers::grok_build;
use serde_json::Value;

use crate::result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord};
use crate::NativeJsonlRuntime;

pub const GROK_BUILD_SOURCE_FORMAT: &str = "grok_build_session_updates_jsonl";

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v9-record-admission-order";

pub const fn grok_build_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::GrokBuild,
        GROK_BUILD_SOURCE_FORMAT,
        "grok-build-acp-updates-jsonl-v1",
        PARSER_REVISION,
    )
}

pub(crate) fn grok_build_file_is_selected(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("updates.jsonl")
}

pub(super) use grok_build::{
    contentless_result_evidence as grok_build_contentless_result_evidence,
    event_identity as grok_build_event_identity, event_text as grok_build_event_text,
    event_type as grok_build_event_type, header_session_id as grok_build_header_session_id,
    role as grok_build_role, structured_tool_call_text as grok_build_structured_tool_call_text,
    timestamp as grok_build_timestamp,
};

pub(super) fn enumerate_grok_build_results(
    value: &Value,
) -> Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    Ok(grok_build::enumerate_results(value)
        .into_iter()
        .map(|record| NativeJsonlResultSubrecord {
            subrecord_index: record.subrecord_index,
            content: record.content,
            call_id: record.call_id,
            tool_name: record.tool_name,
        })
        .collect())
}

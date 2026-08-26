mod metadata;
pub(crate) mod native_path;
mod normalization;
mod source;

pub(crate) use native_path::{mux_jsonl_adapter, mux_jsonl_adapter_with_source_root_lineage};

const MUX_MAX_ID_BYTES: usize = 4 * 1024;
const MUX_MAX_FAILURE_BYTES: usize = 4 * 1024;
const MUX_SOURCE_FORMAT: &str = "mux_session_jsonl";
const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;

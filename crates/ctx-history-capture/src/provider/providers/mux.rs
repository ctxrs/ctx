mod metadata;
pub(crate) mod native_path;
mod normalization;
mod source;

pub(crate) use native_path::mux_jsonl_adapter;

const MUX_MAX_ID_BYTES: usize = 4 * 1024;
const MUX_MAX_FAILURE_BYTES: usize = 4 * 1024;

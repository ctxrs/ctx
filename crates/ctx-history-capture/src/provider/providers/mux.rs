mod metadata;
pub(crate) mod native_path;
mod normalization;
mod source;

pub(crate) use native_path::mux_jsonl_adapter;
pub(crate) use normalization::{mux_event_id, mux_event_text, mux_event_type};

const MUX_MAX_ID_BYTES: usize = 4 * 1024;
const MUX_MAX_FAILURE_BYTES: usize = 4 * 1024;

mod assistant;
pub(crate) mod nativepath;
mod session_tree;
mod source;

pub(crate) use assistant::{
    junie_buffer_result_text, junie_merge_buffered_agent_event, JunieAssistantBuffer,
};
pub(crate) use nativepath::import_junie_nativepath;

const JUNIE_SOURCE_REVISION_SCHEMA: &str = "junie-session-events-v2";
// Match the existing Junie discovery probe's physical index-entry budget.
const MAX_JUNIE_INDEX_ENTRIES: usize = 10_000;
// An entire Junie index gets the same byte allowance as one provider JSONL
// record. The checked-in fixture is one 167-byte entry.
const MAX_JUNIE_INDEX_BYTES: usize = crate::MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_JUNIE_INDEX_METADATA_BYTES: usize = 32 * 1024;
const MAX_JUNIE_TRANSIENT_TURN_BYTES: usize = crate::MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_JUNIE_FAILURES: usize = 16;
const MAX_JUNIE_FAILURE_BYTES: usize = 4 * 1024;

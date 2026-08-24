mod discovery;
mod dto;
mod parser;
mod raw_json;
mod source_backed;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiEventIdentity, GeminiFileObservation, GeminiRetainedEvent, GeminiScanError,
    GeminiSession, GeminiTranscriptSource,
};
pub use source_backed::gemini_jsonl_adapter;
#[cfg(any(test, feature = "test-support"))]
pub use source_backed::{
    gemini_legacy_v1_jsonl_adapter_for_test, install_after_gemini_recording_discovery_hook,
};

#[cfg(test)]
mod tests;

mod discovery;
mod dto;
mod file_invocation;
mod parser;
mod source_backed;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiEventIdentity, GeminiFileObservation, GeminiRetainedEvent, GeminiScanError,
    GeminiSession, GeminiTranscriptSource,
};
pub(crate) use source_backed::registration::register as register_source_backed_route;

#[cfg(test)]
mod tests;

mod discovery;
mod dto;
mod parser;
mod source_backed;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiCheckpoint, GeminiEventIdentity, GeminiFileObservation, GeminiRetainedEvent,
    GeminiScanError, GeminiSession, GeminiTranscriptSource,
};
pub(crate) use parser::{GeminiNativePage, GeminiNativePageReader};
pub(crate) use source_backed::registration::register as register_source_backed_route;

#[cfg(test)]
mod tests;

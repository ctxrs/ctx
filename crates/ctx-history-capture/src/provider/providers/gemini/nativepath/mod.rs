mod discovery;
mod dto;
mod parser;
mod source_backed;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiCheckpoint, GeminiEventIdentity, GeminiFileObservation, GeminiPageFrontier,
    GeminiPageIdentity, GeminiPreviousSource, GeminiRetainedEvent, GeminiScanError,
    GeminiScanOutcome, GeminiSession, GeminiSourceChange, GeminiTranscriptSource,
    GEMINI_NATIVEPATH_PARSER_REVISION, GEMINI_NATIVEPATH_POLICY_REVISION,
};
pub(crate) use parser::{read_gemini_transcript_pages, GeminiNativePage, GeminiNativePageReader};
// This is the provider-local handoff surface for the shared coordinator; its
// caller lands separately from provider adapters.
pub(crate) use source_backed::registration::register as register_source_backed_route;
#[allow(unused_imports)]
pub(crate) use source_backed::{
    hydrate_gemini_source_backed_record, GeminiHydratedSourceRecord, GeminiSourceBackedError,
    GeminiSourceBackedLeaf, GeminiSourceBackedLeafReader, GeminiSourceBackedPage,
    GeminiSourceBackedResult,
};

#[cfg(test)]
mod tests;

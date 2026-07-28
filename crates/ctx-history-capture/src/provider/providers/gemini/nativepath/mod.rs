mod discovery;
mod dto;
mod parser;
mod source_backed;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiCheckpoint, GeminiEventIdentity, GeminiFileObservation, GeminiNativePathProfile,
    GeminiPageFrontier, GeminiPageIdentity, GeminiPreviousSource, GeminiRetainedEvent,
    GeminiScanError, GeminiScanOutcome, GeminiSession, GeminiSourceChange, GeminiTranscriptSource,
    GEMINI_NATIVEPATH_PARSER_REVISION, GEMINI_NATIVEPATH_POLICY_REVISION,
};
pub(crate) use parser::{
    read_gemini_transcript_pages_with_profile, GeminiNativePage, GeminiNativePageReader,
};
pub(crate) use source_backed::{
    hydrate_gemini_source_backed_record, GeminiHydratedSourceRecord, GeminiSourceBackedError,
    GeminiSourceBackedLeaf, GeminiSourceBackedLeafReader, GeminiSourceBackedPage,
    GeminiSourceBackedResult,
};

#[cfg(test)]
mod tests;

mod discovery;
mod dto;
mod parser;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiCheckpoint, GeminiEventIdentity, GeminiFileObservation, GeminiNativePathProfile,
    GeminiPageFrontier, GeminiPageIdentity, GeminiPreviousSource, GeminiRetainedEvent,
    GeminiScanError, GeminiScanOutcome, GeminiSession, GeminiSourceChange, GeminiTranscriptSource,
    GEMINI_NATIVEPATH_PARSER_REVISION, GEMINI_NATIVEPATH_POLICY_REVISION,
};
pub(crate) use parser::{read_gemini_transcript_pages_with_profile, GeminiNativePage};

#[cfg(test)]
mod tests;

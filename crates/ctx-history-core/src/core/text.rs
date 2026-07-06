#[allow(unused_imports)]
use super::*;

text_enum! {
    /// Payload handling state.
    ///
    /// The serialized value `safe_preview` is legacy contract spelling for a
    /// local searchable preview. It is not a promise that output is share-safe.
    pub enum RedactionState {
        Raw => "raw",
        Redacted => "redacted",
        LocalPreview => "safe_preview",
        Withheld => "withheld",
    }
    default LocalPreview
}

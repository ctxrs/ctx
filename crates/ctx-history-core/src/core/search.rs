#[allow(unused_imports)]
use super::*;

impl RedactionState {
    /// Compatibility alias for the legacy Rust API name.
    ///
    /// New code should prefer `LocalPreview`, which better matches the local
    /// search contract while preserving the serialized `safe_preview` value.
    #[allow(non_upper_case_globals)]
    pub const SafePreview: Self = Self::LocalPreview;
}

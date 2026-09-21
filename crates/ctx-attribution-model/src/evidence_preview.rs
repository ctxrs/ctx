use serde::{Deserialize, Serialize};

pub const MAX_EVIDENCE_PREVIEW_CITATIONS: usize = 3;
pub const MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryFileInvocationKind {
    Read,
    Create,
    Modify,
    Delete,
    Rename,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidencePreviewModel {
    pub previews: Vec<EvidencePreview>,
}

/// Provider-neutral evidence for one exact provider-native file-operation request.
///
/// `operation` describes requested intent, not a successful filesystem effect. `excerpt` is an
/// exact UTF-8 byte range copied from `CoreContent::normalized_body`; presentation sanitization is
/// deliberately separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidencePreview {
    pub citation_numbers: Vec<u32>,
    pub operation: RepositoryFileInvocationKind,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_path: Option<String>,
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_occurred_at_ms: Option<i64>,
    pub excerpt: String,
}

use super::*;

pub(super) const MUX_FRONTIER_VERSION: u32 = 1;
pub(super) const MUX_PAGE_MAX_RECORDS: usize = 8;
pub(super) const MUX_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MUX_MAX_FILE_TOUCHES_PER_EVENT: usize = 448;
pub(super) const MUX_PARTIAL_NATIVE_ORDINAL: u64 = 1_u64 << 63;
pub(super) const MUX_GENERATION_BITS: u32 = 16;
pub(super) const MUX_ORDINAL_BITS: u32 = 47;
pub(super) const MUX_MAX_GENERATION: u64 = (1_u64 << MUX_GENERATION_BITS) - 1;
pub(super) const MUX_MAX_ORDINAL: u64 = (1_u64 << MUX_ORDINAL_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MuxStreamKind {
    Chat,
    Partial,
}

impl MuxStreamKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat-jsonl",
            Self::Partial => "partial-json",
        }
    }

    pub(super) fn is_partial(self) -> bool {
        self == Self::Partial
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxFrontier {
    pub(super) version: u32,
    pub(super) next_offset: u64,
    pub(super) next_ordinal: u64,
    pub(super) prefix_sha256: [u8; 32],
    pub(super) file_identity: Option<String>,
}

impl MuxFrontier {
    pub(super) fn initial() -> Self {
        Self {
            version: MUX_FRONTIER_VERSION,
            next_offset: 0,
            next_ordinal: 0,
            prefix_sha256: Sha256::digest([]).into(),
            file_identity: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct MuxFailure {
    // Preserve exact rejected-line diagnostics in the bounded page shape.
    #[allow(dead_code)]
    pub(super) line: usize,
    #[allow(dead_code)]
    pub(super) error: String,
}

#[derive(Debug)]
pub(super) struct MuxPreparedRow {
    pub(super) source_record_ordinal: u64,
    pub(super) source_locator: CompleteContentSourceLocator,
    pub(super) source_record_digest: CompleteContentBodyDigest,
    pub(super) native_record_id: String,
    pub(super) message_content_ref: Option<ContentRef>,
    pub(super) unaddressable_output: Option<MuxUnaddressableOutput>,
    pub(super) event: Option<MuxCoreEvent>,
    pub(super) file_touches: Vec<MuxFileTouch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MuxUnaddressableOutput {
    Redacted,
    Missing,
}

#[derive(Debug)]
pub(super) struct MuxFileTouch {
    pub(super) path: String,
}

#[derive(Debug)]
pub(super) struct MuxPreparedPage {
    pub(super) rows: Vec<MuxPreparedRow>,
    pub(super) next: MuxFrontier,
    pub(super) terminal: bool,
    pub(super) deferred_incomplete: bool,
    pub(super) rejected_records: u64,
    pub(super) first_failure: Option<MuxFailure>,
}

#[derive(Debug)]
pub(super) struct MuxSourcePlan {
    pub(super) path: PathBuf,
    pub(super) kind: MuxStreamKind,
    pub(super) observation: MuxFileObservation,
    pub(super) generation: u64,
}

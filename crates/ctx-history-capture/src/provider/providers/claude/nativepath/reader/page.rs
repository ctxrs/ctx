use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudePageCertificate {
    pub(crate) canonical_route: std::path::PathBuf,
    pub(crate) observation_sha256: [u8; 32],
    pub(crate) physical_file_id: Option<ClaudePhysicalFileId>,
    pub(crate) certified_prefix_end: u64,
    pub(crate) certified_prefix_chain_sha256: [u8; 32],
}

#[derive(Debug)]
pub(crate) struct ClaudeNativePage {
    pub(crate) session: ClaudeSessionMetadata,
    pub(crate) rows: Vec<ClaudeRetainedRow>,
}

#[derive(Debug)]
pub(crate) struct ParseOutput {
    pub(crate) change: ChangeSignal,
    pub(crate) rejections: RejectionSummary,
    pub(crate) checkpoint: ParseCheckpoint,
    pub(crate) stats: ParseStats,
}

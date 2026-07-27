use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeNativeProfile {
    CoreOnly,
    CoreAndPro,
    ProReplayOnly,
}

impl ClaudeNativeProfile {
    pub(super) fn includes_core(self) -> bool {
        matches!(self, Self::CoreOnly | Self::CoreAndPro)
    }

    pub(super) fn includes_pro(self) -> bool {
        matches!(self, Self::CoreAndPro | Self::ProReplayOnly)
    }

    pub(super) fn record_mode(self) -> ClaudeRecordMode {
        match self {
            Self::CoreOnly => ClaudeRecordMode::CoreOnly,
            Self::CoreAndPro => ClaudeRecordMode::CoreAndPro,
            Self::ProReplayOnly => ClaudeRecordMode::ProReplayOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IncompleteTail {
    pub(crate) byte_start: u64,
    pub(crate) observed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudePageCertificate {
    pub(crate) canonical_route: std::path::PathBuf,
    pub(crate) observation_sha256: [u8; 32],
    pub(crate) physical_file_id: Option<ClaudePhysicalFileId>,
    pub(crate) certified_prefix_end: u64,
    pub(crate) certified_prefix_chain_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaudeNativePageIdentity(pub(super) [u8; 32]);

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ClaudeNativePage {
    pub(crate) identity: ClaudeNativePageIdentity,
    pub(crate) session: ClaudeSessionMetadata,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) next_safe_frontier: ClaudeNativeFrontier,
    pub(crate) rows: Vec<ClaudeRetainedRow>,
    pub(crate) rejections: Vec<RecordRejection>,
    pub(crate) rejected_records: u64,
    pub(crate) logical_units: usize,
    pub(crate) serialized_bytes: usize,
    pub(crate) terminal: bool,
    pub(crate) certificate: ClaudePageCertificate,
}

impl ClaudeNativePage {
    #[allow(dead_code)]
    pub(crate) fn receipt(&self) -> ClaudeNativePageReceipt {
        ClaudeNativePageReceipt {
            identity: self.identity,
            expected_frontier: self.expected_frontier.clone(),
            committed_frontier: self.next_safe_frontier.clone(),
            accepted_rows: self.rows.len(),
            accepted_physical_records: self.logical_units,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ClaudeNativePageReceipt {
    pub(crate) identity: ClaudeNativePageIdentity,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) committed_frontier: ClaudeNativeFrontier,
    pub(crate) accepted_rows: usize,
    pub(crate) accepted_physical_records: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaudeNativeProOutputPageIdentity(pub(super) [u8; 32]);

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ClaudeNativeProOutputPage {
    pub(crate) identity: ClaudeNativeProOutputPageIdentity,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) next_safe_frontier: ClaudeNativeFrontier,
    pub(crate) outputs: Vec<ProOutputObservation>,
    pub(crate) rejections: Vec<RecordRejection>,
    pub(crate) rejected_outputs: u64,
    pub(crate) logical_units: usize,
    pub(crate) serialized_bytes: usize,
    pub(crate) terminal: bool,
    pub(crate) certificate: ClaudePageCertificate,
}

#[derive(Debug)]
pub(crate) enum ClaudeNativeOwnedPage {
    Core(Box<ClaudeNativePage>),
    Pro(Box<ClaudeNativeProOutputPage>),
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ParseOutput {
    pub(crate) change: ChangeSignal,
    pub(crate) lifecycle: ClaudeSourceLifecycle,
    pub(crate) rejections: RejectionSummary,
    pub(crate) pro_rejections: RejectionSummary,
    pub(crate) session: ClaudeSessionMetadata,
    pub(crate) checkpoint: ParseCheckpoint,
    pub(crate) incomplete_tail: Option<IncompleteTail>,
    pub(crate) stats: ParseStats,
    pub(crate) source_certified: bool,
}

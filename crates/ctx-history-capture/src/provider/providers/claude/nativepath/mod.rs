//! Provider-owned Claude Code discovery, parsing, and Store publication.

mod checkpoint;
mod privacy;
mod reader;
mod record;
mod rows;
mod source;
mod vertical;

pub(crate) use checkpoint::ParseCheckpoint;
pub(crate) use reader::{
    ClaudeNativeOwnedPage, ClaudeNativePage, ClaudeNativeProOutputPage, ClaudeNativeProfile,
    ClaudeNativeScanner,
};
pub(crate) use rows::{ClaudeEventKind, ClaudeRetainedRow, ClaudeSessionMetadata};
pub(crate) use source::{
    discover_projects, revalidate_discovered_source, ClaudeNativePathError,
    DiscoveredClaudeSession, SessionLayout,
};
pub(crate) use vertical::import_claude_nativepath_projects;

#[cfg(test)]
pub(crate) use checkpoint::{ChangeSignal, ClaudeNativeFrontier};
#[cfg(test)]
pub(crate) use reader::{
    parse_session, ClaudeNativePageIdentity, ClaudeNativePageReceipt,
    ClaudeNativeProOutputPageIdentity, ClaudeNativeProOutputPageReceipt, ClaudePageCertificate,
    IncompleteTail, ParseOutput,
};
#[cfg(test)]
pub(crate) use rows::{
    ClaudeEventIdentity, ClaudeFileTouch, ClaudeNativeOrder, ClaudeOutputOutcome,
    ClaudePhysicalLocator, ClaudeRowPage, ClaudeSparseOutputDiagnostic, ParseStats,
    RecordRejection, RejectionKind, RejectionSummary, ToolCallRequest, CLAUDE_MAX_PAGE_BYTES,
    CLAUDE_MAX_PAGE_ROWS, CLAUDE_MAX_REJECTION_SAMPLES,
};
#[cfg(test)]
pub(crate) use source::{
    authoritative_deletion_candidates, ClaudeDeletionCandidate, ClaudeDiscovery,
    ClaudeDiscoveryStats, ClaudeFileFingerprint, ClaudeInventoryCertificate, ClaudeSessionKey,
    ClaudeSourceLifecycle,
};

#[cfg(test)]
mod tests;

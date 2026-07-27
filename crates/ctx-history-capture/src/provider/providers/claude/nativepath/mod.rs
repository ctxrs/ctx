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
pub(crate) use checkpoint::ChangeSignal;
#[cfg(test)]
pub(crate) use reader::{parse_session, ParseOutput};
#[cfg(test)]
pub(crate) use rows::{
    ClaudeOutputOutcome, ClaudePhysicalLocator, RecordRejection, RejectionKind,
    CLAUDE_MAX_PAGE_BYTES, CLAUDE_MAX_PAGE_ROWS, CLAUDE_MAX_REJECTION_SAMPLES,
};
#[cfg(test)]
pub(crate) use source::{authoritative_deletion_candidates, ClaudeSourceLifecycle};

#[cfg(test)]
mod tests;

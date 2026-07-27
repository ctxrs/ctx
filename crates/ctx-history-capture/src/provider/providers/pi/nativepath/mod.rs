//! Provider-owned Pi session discovery, bounded fanout, and Store lifecycle.

mod checkpoint;
mod reader;
mod rows;
mod source;
mod vertical;

pub(crate) use checkpoint::PiNativeCheckpoint;
pub(super) use reader::{
    open_pi_native_session, PiNativeDeleted, PiNativeOpenOutcome, PiNativeOwnedPage,
    PiNativeProfile, PiNativeResume, PiNativeScanOptions, PiNativeScanOutcome, PiNativeScanStats,
    PiNativeScanner, PiSourceLifecycle,
};
pub(super) use rows::{
    PiNativeCorePage, PiNativeCoreUnit, PiNativeEventRow, PiNativeFileTouchRow,
    PiNativePhysicalLocator, PiNativeRejection, PiNativeRejectionKind, PiNativeSessionRow,
    PI_NATIVE_PAGE_MAX_BYTES, PI_NATIVE_PAGE_MAX_UNITS,
};
pub(super) use source::{
    discover_pi_sessions, revalidate_pi_source_revision, PiDiscovery, PiDiscoveryStats,
    PiNativePathError, PiPhysicalFileId,
};
pub(crate) use vertical::import_pi_nativepath_history;

#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod tests;

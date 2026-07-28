//! Provider-owned Pi session discovery, bounded fanout, and Store lifecycle.

mod checkpoint;
mod reader;
mod rows;
mod source;
mod source_backed;
mod vertical;

pub(crate) use checkpoint::PiNativeCheckpoint;
pub(super) use reader::{
    open_pi_native_session, open_pi_native_session_retained, PiNativeOpenOutcome,
    PiNativeOwnedPage, PiNativeProfile, PiNativeResume, PiNativeScanOptions, PiSourceLifecycle,
};
pub(super) use rows::{
    PiNativeCorePage, PiNativeCoreUnit, PiNativeEventRow, PiNativeFileTouchRow, PiNativeSessionRow,
};
pub(super) use source::{discover_pi_sessions, revalidate_pi_source_revision, PiDiscovery};
pub(crate) use source_backed::{
    project_pi_source_backed_root_cold, PiSourceBackedError, PiSourceBackedPage,
    PiSourceBackedProjection, PiSourceBackedResolver, PiSourceBackedRoot,
    PiSourceBackedRootProjection, PiSourceBackedScanner, PiSourceRoute,
};
pub(crate) use vertical::import_pi_nativepath_history;

#[cfg(test)]
use reader::PiNativeScanOutcome;
#[cfg(test)]
use rows::{
    PiNativeRejection, PiNativeRejectionKind, PI_NATIVE_PAGE_MAX_BYTES, PI_NATIVE_PAGE_MAX_UNITS,
};
#[cfg(test)]
use source::PiNativePathError;

#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod source_backed_tests;
#[cfg(test)]
mod tests;

//! Selected/direct SQLite provider implementations for ctx history capture.
//!
//! Firebender, Goose, Kiro, and Warp own their native parsing, immutable
//! snapshot interpretation, source identities, and replacement-tree adapters
//! here. Concrete generation publication remains in the capture façade.

mod error;
mod native_source;
mod providers;
mod record_evidence;

pub use error::{CaptureError, Result};
pub use providers::{
    firebender_source_backed_driver, firebender_source_backed_driver_scoped,
    goose_source_backed_driver, goose_source_backed_driver_scoped, kiro_source_backed_driver,
    kiro_source_backed_driver_scoped, warp_source_backed_driver, warp_source_backed_driver_scoped,
    GooseSourceRoute,
};

pub use ctx_history_capture_runtime::{
    CaptureLifecycleSink, DocumentRecordSpool, SourceBackedRouteDriver,
};
pub use ctx_history_source_sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES;

pub const FIREBENDER_SQLITE_SOURCE_FORMAT: &str = "firebender_chat_history_sqlite";
pub const GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT: &str = "goose_sessions_sqlite";
pub const KIRO_SQLITE_SOURCE_FORMAT: &str = "kiro_cli_sqlite";
pub const WARP_SQLITE_SOURCE_FORMAT: &str = "warp_sqlite";

#[cfg(feature = "test-support")]
pub fn fail_next_opened_snapshot_cleanup_for_test() {
    ctx_history_source_sqlite::fail_next_opened_snapshot_cleanup_for_test();
}

const NATIVE_INGESTION_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

fn document_inventory_authority(
    provider: &str,
    source_format: &str,
    path: &std::path::Path,
) -> ctx_history_capture_runtime::DocumentInventoryAuthority {
    use sha2::{Digest, Sha256};

    let path = path.as_os_str().as_encoded_bytes();
    let mut digest = Sha256::new();
    digest.update(b"ctx.document-tree-route-authority-v1\0");
    digest.update((provider.len() as u64).to_be_bytes());
    digest.update(provider.as_bytes());
    digest.update((source_format.len() as u64).to_be_bytes());
    digest.update(source_format.as_bytes());
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    ctx_history_capture_runtime::DocumentInventoryAuthority::new(
        provider.to_owned(),
        digest.finalize().into(),
    )
}

/// Concrete lifecycle binding supplied by the capture composition root.
///
/// Provider packs own adapters but never an index or publication authority.
/// This associated-type port lets the façade select its one lifecycle and
/// bounded spool without introducing an upward dependency.
pub trait SelectedSqliteCaptureBinding: Send + Sync + 'static {
    type Lifecycle: CaptureLifecycleSink;
    type Spool: DocumentRecordSpool;
    type RouteControl: Send + Sync + 'static;
}

pub(crate) mod common {
    pub(crate) mod io {
        pub(crate) use ctx_history_source_io::*;
    }
}

pub(crate) mod provider_sources {
    pub(crate) use ctx_history_source_sqlite::*;
}

pub(crate) mod provider {
    pub(crate) mod sqlite {
        pub(crate) use ctx_history_source_sqlite::*;
    }

    pub(crate) mod source_backed {
        pub(crate) use ctx_history_capture_runtime::*;

        pub(crate) fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                error.to_string(),
            )
        }

        pub(crate) fn combine_primary_and_cleanup_route_errors(
            primary: SourceBackedRouteError,
            cleanup: SourceBackedRouteError,
        ) -> SourceBackedRouteError {
            let kind = if route_error_severity(primary.kind) >= route_error_severity(cleanup.kind) {
                primary.kind
            } else {
                cleanup.kind
            };
            SourceBackedRouteError::new(
                kind,
                format!(
                    "{}; explicit SQLite snapshot cleanup also failed: {}",
                    primary.detail, cleanup.detail
                ),
            )
        }

        const fn route_error_severity(kind: SourceBackedRouteErrorKind) -> u8 {
            match kind {
                SourceBackedRouteErrorKind::Internal => 6,
                SourceBackedRouteErrorKind::ResourceUnavailable => 5,
                SourceBackedRouteErrorKind::SourceChanged => 4,
                SourceBackedRouteErrorKind::InvalidSource => 3,
                SourceBackedRouteErrorKind::Unsupported => 2,
                SourceBackedRouteErrorKind::Unavailable => 1,
            }
        }
    }
}

#[cfg(test)]
fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;

    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| test_support_paths::tempdir().expect("provider SQLite test root"))
        .path()
}

#[cfg(test)]
mod test_support_paths {
    pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new().prefix("ctx-test-").tempdir()
    }
}

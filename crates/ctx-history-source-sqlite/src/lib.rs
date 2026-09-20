//! Provider-neutral SQLite acquisition and immutable read snapshots.
//!
//! This crate owns root-authorized DB/WAL-family acquisition, bounded scratch
//! copies, read-only connection hardening, physical evidence, logical SQLite
//! snapshot identities, and failure classification. Provider registration and
//! publication policy remain in their owning runtime crates.

#![cfg_attr(feature = "test-support", allow(dead_code, unused_imports))]

mod error;
mod logical;
mod native_value;
mod progress;
mod query;
mod sqlite;
mod sqlite_source;
mod value;

pub use error::{Result, SqliteIoError};
pub use logical::SqliteLogicalSnapshot;
pub use native_value::sqlite_logical_record_digest_bytes;
pub use progress::{SqliteSourceProgress, SqliteSourceProgressStage};
pub use query::*;
pub use sqlite::*;
pub use sqlite_source::*;
pub use value::NativeSqliteValue;

pub const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(any(test, feature = "test-support"))]
mod test_support_paths;

#[cfg(any(test, feature = "test-support"))]
fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;

    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| test_support_paths::tempdir().expect("provider SQLite test root"))
        .path()
}

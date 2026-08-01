//! Kiro's provider-owned NativePath ingestion leaf.
//!
//! The provider entry point below routes solely to its NativePath driver.
//! There is no alternate producer, projector, coordinator, or runtime fallback.

mod event;
mod history;
pub(crate) mod native_path;

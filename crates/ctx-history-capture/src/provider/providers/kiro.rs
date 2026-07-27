//! Kiro's provider-owned NativePath ingestion leaf.
//!
//! The provider entry point below routes solely to its NativePath driver.
//! There is no alternate producer, projector, coordinator, or runtime fallback.

use std::path::Path;

use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod event;
mod history;
mod native_path;

#[cfg(test)]
pub(crate) use event::KiroNativeEvent;
pub(crate) use history::{
    decode_kiro_conversation_for_complete, kiro_history_events, kiro_provider_session_id,
    kiro_session_started_at,
};

pub(crate) fn import_kiro_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_kiro_native_path(path, store, context, options)
}
